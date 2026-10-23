//! Adobe Premiere Pro effect preset (`.prfpset`) reader.
//!
//! A `.prfpset` is an XML `<PremiereData>` object graph: a bin tree whose leaves are
//! `FilterPresetItem`s. Each item holds one or more `FilterPreset`s, and each `FilterPreset`
//! names an effect (`FilterMatchName`) and points at a `VideoFilterComponent` /
//! `AudioFilterComponent` carrying the parameter values and keyframes.
//!
//! FilmCraft has no plugin API, so this reader maps Premiere effects and parameters onto the
//! built-in ones:
//!
//! * effects: known match names first, then a normalised comparison with the built-in display
//!   names (so `AE.ADBE Black & White`, `AE.BCC_BLACKANDWHITE` and a component display name of
//!   `BCC+Black and White` all reach the same effect);
//! * parameters: stable Premiere parameter ids where we know them, otherwise parameter names in
//!   English and Spanish (and common synonyms), so localised presets work too.
//!
//! A preset whose effects have no FilmCraft counterpart is *skipped*, not fatal: the import
//! reports which effects were missing (Premiere itself refuses to apply a preset when a third-party
//! plugin is not installed). Parameters the target effect does not have are reported per preset.
//!
//! The reader treats the file as hostile input: bounded recursion, checked arithmetic, no panics,
//! a size cap, and an actionable error for anything it cannot read.

use std::collections::{HashMap, HashSet};

use filmcraft_geom::Vec2;
use filmcraft_project::effect::{find_effect_by_name, LIGHT_IDS};
use filmcraft_project::graphic::is_layer_id;
use filmcraft_project::{
    EffectInstance, Interpolation, Keyframe, Param, ParamDef, ParamKind, ParamValue, find_effect,
};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use roxmltree::{Document, Node};

use crate::presets::{EffectPreset, KeyframeMode};

/// Largest preset file this reader accepts. Real packs are a few megabytes; the cap keeps a
/// corrupt or hostile file from exhausting memory.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
/// A parameter in a pathological file cannot make us allocate unbounded keyframes.
const MAX_KEYFRAMES: usize = 50_000;
const DEFAULT_DURATION: i64 = 5 * TICKS_PER_SECOND;

/// One preset whose effects FilmCraft cannot reproduce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedPreset {
    pub name: String,
    /// The effects as Premiere names them, e.g. `BCC+Black and White (AE.BCC_BLACKANDWHITE)`.
    pub effects: Vec<String>,
    pub reason: String,
}

/// Everything one `.prfpset` file yielded.
#[derive(Clone, Debug, Default)]
pub struct PrfpsetImport {
    pub presets: Vec<EffectPreset>,
    pub skipped: Vec<SkippedPreset>,
    /// One line per preset that lost parameters the target effect does not have.
    pub warnings: Vec<String>,
}

impl PrfpsetImport {
    /// Human-readable summary used by the import command and the status bar.
    pub fn summary(&self) -> String {
        let mut s = format!("{} preset{}", self.presets.len(), if self.presets.len() == 1 { "" } else { "s" });
        if !self.skipped.is_empty() {
            s.push_str(&format!(", {} skipped", self.skipped.len()));
        }
        if !self.warnings.is_empty() {
            s.push_str(&format!(", {} with unsupported parameters", self.warnings.len()));
        }
        s
    }
}

type Objects<'a, 'input> = HashMap<&'input str, Node<'a, 'input>>;

/// Parse a Premiere `.prfpset` XML document.
pub fn parse_prfpset(text: &str) -> Result<PrfpsetImport, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let doc = Document::parse(text).map_err(|e| format!("not valid XML: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err("not a Premiere Pro preset file (expected a <PremiereData> document)".into());
    }

    let mut objects: Objects<'_, '_> = HashMap::new();
    for node in doc.descendants() {
        if let Some(id) = node.attribute("ObjectID") {
            objects.insert(id, node);
        }
    }
    if objects.is_empty() {
        return Err("the Premiere preset file has no objects".into());
    }

    let entries = collect_entries(&doc, &objects);
    if entries.is_empty() {
        return Err("no presets found in the Premiere preset file".into());
    }

    let mut out = PrfpsetImport::default();
    let mut seen = HashSet::new();
    let mut pending: Vec<(usize, Vec<String>)> = Vec::new();
    for Entry { name, item_ref } in entries {
        if !seen.insert(item_ref.clone()) {
            continue;
        }
        let Some(item) = objects.get(item_ref.as_str()).copied() else { continue };
        if item.tag_name().name() != "FilterPresetItem" {
            continue;
        }
        match parse_item(&name, item, &objects) {
            Ok((preset, dropped)) => {
                if !dropped.is_empty() {
                    pending.push((out.presets.len(), dropped));
                }
                out.presets.push(preset);
            }
            Err(skip) => out.skipped.push(skip),
        }
    }

    // Premiere allows two presets with the same name in the same bin. Keep both by giving the
    // later ones a numeric suffix (the import is still idempotent because the suffix is stable).
    let mut names: HashSet<String> = HashSet::new();
    for preset in &mut out.presets {
        if names.insert(preset.name.clone()) {
            continue;
        }
        let base = preset.name.clone();
        for n in 2..10_000u32 {
            let candidate = format!("{base} ({n})");
            if names.insert(candidate.clone()) {
                preset.name = candidate;
                break;
            }
        }
    }
    for (index, dropped) in pending {
        if let Some(preset) = out.presets.get(index) {
            let shown = dropped.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
            let more = if dropped.len() > 6 { format!(" and {} more", dropped.len() - 6) } else { String::new() };
            out.warnings.push(format!("{}: parameters not supported by FilmCraft: {shown}{more}", preset.name));
        }
    }

    if out.presets.is_empty() && out.skipped.is_empty() {
        return Err("no presets found in the Premiere preset file".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------- tree walk

/// A preset leaf: its folder-qualified name and the `FilterPresetItem` object it points at.
struct Entry {
    name: String,
    item_ref: String,
}

/// Walk the document tree or recoverable `TreeItem` nodes to collect all preset entries.
fn collect_entries<'a, 'input>(doc: &'a Document<'input>, objects: &Objects<'a, 'input>) -> Vec<Entry> {
    let mut out = Vec::new();
    let tree = doc.descendants().find(|n| n.tag_name().name() == "Tree" && n.attribute("ObjectID").is_some());
    if let Some(tree) = tree
        && let Some(root_ref) = tree.children().find(|n| n.tag_name().name() == "RootBin").and_then(|n| n.attribute("ObjectRef"))
        && let Some(root) = objects.get(root_ref)
    {
        let mut seen = HashSet::new();
        let mut path = Vec::new();
        walk_tree(*root, &mut path, objects, &mut seen, 0, &mut out);
    }

    if out.is_empty() {
        // Older or hand-written files may have no usable Tree: recover the names from the
        // TreeItems that point at the FilterPresetItems.
        let mut by_item: HashMap<&str, String> = HashMap::new();
        for item in doc.descendants().filter(|n| n.tag_name().name() == "TreeItem") {
            let Some(data) = data_ref(item) else { continue };
            if let Some(name) = tree_item_name(item) {
                by_item.entry(data).or_insert(name);
            }
        }
        for (id, node) in objects.iter() {
            if node.tag_name().name() != "FilterPresetItem" {
                continue;
            }
            let name = by_item.get(id).cloned().unwrap_or_else(|| format!("Preset {id}"));
            out.push(Entry { name: sanitize_name(&name), item_ref: (*id).to_string() });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.item_ref.cmp(&b.item_ref)));
    }
    out
}

/// Recursively traverse a Premiere bin hierarchy to collect leaf preset items.
fn walk_tree<'a, 'input>(
    node: Node<'a, 'input>,
    path: &mut Vec<String>,
    objects: &Objects<'a, 'input>,
    seen: &mut HashSet<String>,
    depth: usize,
    out: &mut Vec<Entry>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match node.tag_name().name() {
        "BinTreeItem" => {
            let Some(id) = node.attribute("ObjectID") else { return };
            if !seen.insert(id.to_string()) {
                return;
            }
            let name = tree_item_name(node).unwrap_or_default();
            // The outer "Root"/"Presets" containers are Premiere's scaffolding, not user bins.
            let scaffold = is_scaffold_bin(node, &name, path.is_empty());
            let push = !name.is_empty() && !scaffold;
            if push {
                path.push(sanitize_name(&name));
            }
            if let Some(items) = node.children().find(|n| n.tag_name().name() == "Items") {
                for item in items.children().filter(|n| n.tag_name().name() == "Item") {
                    let Some(child_ref) = item.attribute("ObjectRef") else { continue };
                    let Some(child) = objects.get(child_ref) else { continue };
                    walk_tree(*child, path, objects, seen, depth + 1, out);
                }
            }
            if push {
                path.pop();
            }
        }
        "TreeItem" => {
            let Some(id) = node.attribute("ObjectID") else { return };
            if !seen.insert(id.to_string()) {
                return;
            }
            let Some(item_ref) = data_ref(node) else { return };
            let Some(item) = objects.get(item_ref) else { return };
            if item.tag_name().name() != "FilterPresetItem" {
                return;
            }
            let leaf = tree_item_name(node).unwrap_or_else(|| "Preset".to_string());
            let leaf = sanitize_name(&leaf);
            let name = if path.is_empty() { leaf } else { format!("{} ▸ {leaf}", path.join(" ▸ ")) };
            out.push(Entry { name, item_ref: item_ref.to_string() });
        }
        _ => {}
    }
}

/// Determine if a bin is structural scaffold (such as Root or top-level Presets folder).
fn is_scaffold_bin(node: Node<'_, '_>, name: &str, path_is_empty: bool) -> bool {
    if name.eq_ignore_ascii_case("root") {
        return true;
    }
    if path_is_empty {
        if is_presets_bin(name) {
            return true;
        }
        if node.descendants().any(|n| n.tag_name().name() == "HandlerEffects.EffectItemTree.PresetsBin") {
            return true;
        }
    }
    false
}

/// Match localized names of the top-level Premiere Presets folder.
fn is_presets_bin(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "presets"
            | "ajustes preestablecidos"
            | "préconfigurations"
            | "preconfigurations"
            | "vorgaben"
            | "predefiniti"
            | "predefinições"
            | "predefinicoes"
            | "шаблоны настроек"
            | "プリセット"
            | "预设"
            | "預設"
            | "사전 설정"
    )
}

/// Extract the display name from a tree item or its base.
fn tree_item_name(node: Node<'_, '_>) -> Option<String> {
    node.children()
        .find(|n| n.tag_name().name() == "TreeItemBase")
        .and_then(|base| child_text(base, "Name"))
        .or_else(|| child_text(node, "Name"))
}

/// Extract the target object reference ID associated with a tree item.
fn data_ref<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.children()
        .find(|n| n.tag_name().name() == "TreeItemBase")
        .and_then(|base| base.children().find(|n| n.tag_name().name() == "Data"))
        .or_else(|| node.children().find(|n| n.tag_name().name() == "Data"))
        .and_then(|d| d.attribute("ObjectRef"))
}

// ---------------------------------------------------------------- one preset

/// Parse an individual preset item and its associated filter preset components.
fn parse_item<'a, 'input>(
    name: &str,
    item: Node<'a, 'input>,
    objects: &Objects<'a, 'input>,
) -> Result<(EffectPreset, Vec<String>), SkippedPreset> {
    let preset_nodes: Vec<Node<'_, '_>> = if let Some(list) = item.children().find(|n| n.tag_name().name() == "FilterPresets") {
        list.children().filter(|n| n.tag_name().name() == "FilterPreset").collect()
    } else {
        item.children().filter(|n| n.tag_name().name() == "FilterPreset").collect()
    };
    if preset_nodes.is_empty() {
        return Err(SkippedPreset { name: name.to_string(), effects: Vec::new(), reason: "no effects in this preset".into() });
    }

    let mut effects = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut description = String::new();
    let mut mode = KeyframeMode::Scale;
    let mut anchor_in = 0i64;
    let mut anchor_out: Option<i64> = None;
    let mut first = true;
    let mut anchors = false;

    for elem in preset_nodes {
        let fp = match elem.attribute("ObjectRef").and_then(|r| objects.get(r)).copied() {
            Some(n) => n,
            None if elem.attribute("ObjectID").is_some() => elem,
            None => continue,
        };
        let match_name = child_text(fp, "FilterMatchName").unwrap_or_default();
        let match_name = match_name.trim();
        let component = fp
            .children()
            .find(|n| n.tag_name().name() == "Component")
            .and_then(|c| c.attribute("ObjectRef"))
            .and_then(|r| objects.get(r))
            .copied();
        let display = component.and_then(component_display_name).unwrap_or_default();

        let in_point = child_text(fp, "AnchorInPoint").and_then(|s| parse_i64(&s));
        let out_point = child_text(fp, "AnchorOutPoint").and_then(|s| parse_i64(&s));
        if first {
            mode = match child_text(fp, "Type").and_then(|s| parse_i64(&s)) {
                Some(1) => KeyframeMode::AnchorToIn,
                Some(2) => KeyframeMode::AnchorToOut,
                _ => KeyframeMode::Scale,
            };
            description = child_text(fp, "Description").map(|s| s.trim().to_string()).unwrap_or_default();
            first = false;
        }
        if !anchors && (in_point.is_some() || out_point.is_some()) {
            anchor_in = in_point.unwrap_or(0);
            anchor_out = out_point;
            anchors = true;
        }

        if match_name.is_empty() && display.is_empty() {
            continue;
        }
        let Some(effect_id) = resolve_effect(match_name, &display) else {
            unsupported.push(unsupported_name(match_name, &display));
            continue;
        };
        let Some(component) = component else {
            unsupported.push(unsupported_name(match_name, &display));
            continue;
        };
        match parse_component(effect_id, match_name, component, anchor_in, objects) {
            Ok((instance, missing)) => {
                dropped.extend(missing);
                effects.push(instance);
            }
            Err(_) => unsupported.push(unsupported_name(match_name, &display)),
        }
    }

    if effects.is_empty() {
        let reason = if unsupported.is_empty() {
            "the preset has no effect data".to_string()
        } else {
            "none of its effects are supported by FilmCraft".to_string()
        };
        return Err(SkippedPreset { name: name.to_string(), effects: unsupported, reason });
    }

    let duration = match anchor_out {
        Some(out) => {
            let d = out.checked_sub(anchor_in).unwrap_or(DEFAULT_DURATION);
            if d > 0 { d } else { DEFAULT_DURATION }
        }
        None => DEFAULT_DURATION,
    };

    Ok((
        EffectPreset {
            name: name.to_string(),
            description,
            keyframes: mode,
            source_duration: Tick(duration),
            source_size: (1920, 1080),
            effects,
            builtin: false,
        },
        dropped,
    ))
}

/// Extract the human-readable display name from a component node.
fn component_display_name(component: Node<'_, '_>) -> Option<String> {
    component
        .descendants()
        .find(|n| n.tag_name().name() == "DisplayName")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Format a descriptive name for an unsupported effect for warnings and logs.
fn unsupported_name(match_name: &str, display: &str) -> String {
    match (display.trim(), match_name.trim()) {
        ("", "") => "unknown effect".into(),
        ("", m) => m.into(),
        (d, "") => d.into(),
        (d, m) if d.eq_ignore_ascii_case(m) => d.into(),
        // "S_JpegDamage" and "AE.S_JpegDamage" are the same effect; do not repeat it.
        (d, m) if m.to_ascii_lowercase().ends_with(&d.to_ascii_lowercase()) => m.into(),
        (d, m) => format!("{d} ({m})"),
    }
}

// ---------------------------------------------------------------- components

/// Parse a Premiere component node into a FilmCraft `EffectInstance`.
fn parse_component<'a, 'input>(
    effect_id: &str,
    match_name: &str,
    component: Node<'a, 'input>,
    anchor_in: i64,
    objects: &Objects<'a, 'input>,
) -> Result<(EffectInstance, Vec<String>), String> {
    let def = find_effect(effect_id).ok_or_else(|| effect_id.to_string())?;
    let mut instance = def.instance();
    let bypass = component
        .descendants()
        .find(|n| n.tag_name().name() == "Bypass")
        .and_then(|n| n.text())
        .is_some_and(parse_bool);
    if bypass {
        instance.enabled = false;
    }

    if def.params.is_empty() {
        // Effects such as Black & White have no parameters; a third-party component may still
        // carry its own (BCC does), and dropping them is expected rather than a warning.
        return Ok((instance, Vec::new()));
    }

    let mut dropped: Vec<String> = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    let Some(params) = component.descendants().find(|n| n.tag_name().name() == "Params") else {
        return Ok((instance, dropped));
    };
    let is_adbe = match_name.starts_with("AE.ADBE") || match_name.starts_with("ADBE");
    for param in params.children().filter(|n| n.tag_name().name() == "Param") {
        let index = param.attribute("Index").and_then(parse_usize).unwrap_or(0);
        let Some(node) = param.attribute("ObjectRef").and_then(|r| objects.get(r)).copied() else { continue };
        let Some(raw) = RawParam::read(node) else { continue };
        // Known ids that map to nothing on purpose (a duplicate blend-mode control, sampling
        // options Premiere stores but the built-in effect does not expose): ignore silently.
        if is_adbe && matches!(param_by_premiere_id(effect_id, raw.id), Some("")) {
            continue;
        }
        let label = || if raw.name.trim().is_empty() { format!("parameter {}", raw.id) } else { raw.name.trim().to_string() };
        let Some(target) = resolve_param(def, effect_id, match_name, raw.id, index, &raw.name, &used) else {
            dropped.push(label());
            continue;
        };
        used.insert(target.to_string());
        let Some(pdef) = def.param(target) else {
            dropped.push(label());
            continue;
        };
        if let Some(value) = parse_param(pdef, effect_id, raw.id, &raw, anchor_in) {
            instance.params.insert(target.to_string(), value);
        } else {
            dropped.push(label());
        }
    }
    Ok((instance, dropped))
}

struct RawParam {
    id: i32,
    name: String,
    current: Option<String>,
    start: Option<String>,
    keyframes: Option<String>,
}

impl RawParam {
    /// Read raw parameter attributes and keyframe strings from an XML node.
    fn read(node: Node<'_, '_>) -> Option<Self> {
        Some(RawParam {
            id: child_text(node, "ParameterID").and_then(|s| parse_i64(&s)).and_then(|n| i32::try_from(n).ok()).unwrap_or(-1),
            name: child_text(node, "Name").unwrap_or_default(),
            current: child_text(node, "CurrentValue"),
            start: child_text(node, "StartKeyframe"),
            keyframes: child_text(node, "Keyframes"),
        })
    }

    /// Return the static parameter value string, preferring the start keyframe sentinel.
    fn static_value(&self) -> Option<&str> {
        self.start
            .as_deref()
            .and_then(start_keyframe_value)
            .or_else(|| self.current.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    }
}

// ---------------------------------------------------------------- effect mapping

/// Resolve a Premiere match name or display name to a FilmCraft effect ID.
fn resolve_effect(match_name: &str, display: &str) -> Option<&'static str> {
    if let Some(id) = explicit_effect(match_name)
        && let Some(def) = find_effect(id).filter(|d| !is_layer_id(d.id)) {
            return Some(def.id);
        }
    for candidate in [match_name, display] {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        if let Some(def) = find_effect(candidate).or_else(|| find_effect_by_name(candidate))
            && !is_layer_id(def.id) {
                return Some(def.id);
            }
        let key = normalize_effect_name(candidate);
        if key.is_empty() {
            continue;
        }
        if let Some(def) = filmcraft_project::effect_defs()
            .iter()
            .find(|def| !is_layer_id(def.id) && loose_effect_eq(&key, &normalize_effect_name(def.name)))
        {
            return Some(def.id);
        }
    }
    None
}

/// Match names whose stable Premiere name is not the built-in display name.
fn explicit_effect(match_name: &str) -> Option<&'static str> {
    let m = match_name.trim();
    let lower = m.to_ascii_lowercase();
    let id = match lower.as_str() {
        "ae.adbe motion" => "motion",
        "ae.adbe opacity" => "opacity",
        "ae.adbe timeremap" | "ae.adbe time remapping" => "time_remap",
        "ae.adbe geometry2" | "ae.adbe geometry" => "transform",
        "ae.adbe aecrop" => "crop",
        "ae.adbe fast blur" | "ae.adbe fastblur" => "gaussian_blur",
        "ae.adbe motion blur" => "directional_blur",
        "ae.adbe lightingeffect" => "lighting_effects",
        "ae.adbe linear wipe" => "linear_wipe_legacy",
        "ae.adbe 4-color gradient" => "four_color_gradient",
        "ae.adbe black & white" | "ae.adbe black and white" => "black_white",
        "ae.adbe track matte key" => "track_matte",
        "ae.adbe ultra key" => "ultra_key",
        "ae.adbe lens flare" => "lens_flare",
        "ae.adbe ramp" => "ramp",
        "ae.mettle skybox glow" => "vr_glow",
        "ae.mettle skybox chromatic aberrations" => "vr_chromatic_aberrations",
        "ae.mettle skybox digital glitch" => "vr_digital_glitch",
        // Third-party effects with a direct built-in equivalent. Anything else is reported as
        // missing rather than silently approximated.
        "ae.bcc_blackandwhite" | "ae.s_blackandwhite" | "ae.s_blackandwhite2" => "black_white",
        "ae.bcc_glow" | "ae.s_glow" => "wonder_glow",
        "ae.bcc_blur" | "ae.s_blur" => "gaussian_blur",
        "ae.bcc_sharpen" | "ae.s_sharpen" => "sharpen",
        // Audio filters whose Premiere match names are internal identifiers.
        "internal lowpass stereo" | "internal lowpass mono" | "internal lowpass 5.1" => "lowpass",
        "internal highpass stereo" | "internal highpass mono" | "internal highpass 5.1" => "highpass",
        "internal bass stereo" | "internal bass mono" | "internal bass 5.1" => "bass",
        "internal balance" => "balance_a",
        "3792d46f-5036-4be6-ab5a-177622246a5d" => "graphic_eq",
        "7ea99d31-ca8f-454f-acd7-81a258efb336" => "studio_reverb",
        "82882adc-18cb-475b-b237-597576a5e707" => "vocal_enhancer",
        "8edcd326-33de-4a4d-985b-f1bf48ed1427" => "parametric_eq",
        "99c2e75f-0bdb-48d8-8b51-2bac2e26b2cf" => "convolution_reverb",
        "9b66ab13-4d1b-469a-88c6-b807fcf1df16" => "distortion",
        "c6762838-e451-4d22-9c90-b0eaa734605a" => "denoise",
        "d30b6731-3e00-4a11-9650-e816f58a9d12" => "flanger",
        "e0b23f05-f1a7-4ef7-9b50-7ec3e3002058" => "hard_limiter",
        "f76dad9c-77da-477a-9939-eefefc5c23bd" => "dynamics",
        "ffbe710f-cd69-4139-ad26-65603616d9d4" => "deesser",
        _ => {
            let lower = m.to_ascii_lowercase();
            if lower.starts_with("internal ") {
                if lower.contains("lowpass") {
                    return Some("lowpass");
                }
                if lower.contains("highpass") {
                    return Some("highpass");
                }
                if lower.contains("bass") {
                    return Some("bass");
                }
                if lower.contains("balance") {
                    return Some("balance_a");
                }
            }
            return None;
        }
    };
    Some(id)
}

/// Lower-case, alphanumeric-only comparison key: vendor prefixes and the word "and" disappear so
/// `AE.BCC_BLACKANDWHITE`, `BCC+Black and White` and `Black & White` compare equal.
fn normalize_effect_name(name: &str) -> String {
    let mut s = name.trim();
    for prefix in ["AE.", "PR.", "MZ."] {
        if s.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix)) {
            s = &s[prefix.len()..];
            break;
        }
    }
    let mut key = compact(s);
    for vendor in ["adbe", "bcc", "mettle", "skybox", "sapphire", "redgiant", "mz"] {
        if key.starts_with(vendor) {
            key = key[vendor.len()..].to_string();
        }
    }
    while key.ends_with(|c: char| c.is_ascii_digit()) {
        key.pop();
    }
    key.replace("and", "")
}

/// Compare two effect names loosely, handling plurals and legacy suffixes.
fn loose_effect_eq(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // "LightingEffect" / "Lighting Effects": singular/plural in either direction.
    if let Some(stripped) = a.strip_suffix('s')
        && stripped == b {
            return true;
        }
    if let Some(stripped) = b.strip_suffix('s')
        && stripped == a {
            return true;
        }
    // "Linear Wipe" / "Linear Wipe (Legacy)": the legacy variant is the only built-in one.
    a.strip_suffix("legacy").is_some_and(|x| x == b) || b.strip_suffix("legacy").is_some_and(|x| x == a)
}

// ---------------------------------------------------------------- param mapping

/// Resolve a Premiere parameter name or ID to an effect's parameter definition.
fn resolve_param(
    def: &filmcraft_project::EffectDef,
    effect_id: &str,
    match_name: &str,
    id: i32,
    index: usize,
    name: &str,
    used: &HashSet<String>,
) -> Option<&'static str> {
    if match_name.starts_with("AE.ADBE") || match_name.starts_with("ADBE") {
        if let Some(target) = param_by_premiere_id(effect_id, id) {
            if target.is_empty() {
                return None;
            }
            if def.param(target).is_some() && !used.contains(target) {
                return Some(target);
            }
        }
    }
    let key = compact(name);
    if !key.is_empty() {
        if let Some(p) = def
            .params
            .iter()
            .find(|p| !used.contains(p.id) && (compact(p.label) == key || compact(p.id) == key))
        {
            return Some(p.id);
        }
        for target in param_synonyms(effect_id, &key) {
            if def.param(target).is_some() && !used.contains(target) {
                return Some(target);
            }
        }
    }
    // Last resort: a few effects use an unnamed first parameter in Premiere (e.g. Crop's zoom),
    // so fall back to the parameter at the same index only when the names gave us nothing and the
    // effect is small enough that a positional match cannot silently mix up a large parameter list.
    if key.is_empty() && def.params.len() <= 8
        && let Some(p) = def.params.get(index)
            && !used.contains(p.id) {
                return Some(p.id);
            }
    None
}

/// Stable Premiere parameter ids, for effects whose ids are known. `""` means "known, ignore".
fn param_by_premiere_id(effect_id: &str, id: i32) -> Option<&'static str> {
    let target = match effect_id {
        "transform" => match id {
            1 => "anchor",
            2 => "position",
            11 => "uniform_scale",
            3 => "scale_height",
            4 => "scale_width",
            5 => "skew",
            6 => "skew_axis",
            7 => "rotation",
            8 => "opacity",
            9 => "shutter_override",
            10 => "shutter_angle",
            _ => return None,
        },
        "drop_shadow" => match id {
            1 => "color",
            2 => "opacity",
            3 => "direction",
            4 => "distance",
            5 => "softness",
            6 => "only",
            _ => return None,
        },
        "gaussian_blur" => match id {
            1 => "blurriness",
            2 => "dimensions",
            3 => "repeat_edge",
            _ => return None,
        },
        "directional_blur" => match id {
            1 => "direction",
            2 => "length",
            _ => return None,
        },
        "tint" => match id {
            1 => "black",
            2 => "white",
            3 => "amount",
            _ => return None,
        },
        "basic_3d" => match id {
            1 => "swivel",
            2 => "tilt",
            3 => "distance",
            _ => return None,
        },
        "crop" => match id {
            1 => "left",
            2 => "top",
            3 => "right",
            4 => "bottom",
            5 => "zoom",
            _ => return None,
        },
        "mirror" => match id {
            1 => "center",
            2 => "angle",
            _ => return None,
        },
        "lens_distortion" => match id {
            1 => "curvature",
            2 => "v_decentering",
            3 => "h_decentering",
            _ => return None,
        },
        "wave_warp" => match id {
            2 => "height",
            3 => "width",
            4 => "direction",
            5 => "speed",
            _ => return None,
        },
        "vr_glow" => match id {
            2 => "threshold",
            3 => "radius",
            4 => "brightness",
            5 => "saturation",
            6 => "use_tint",
            7 => "tint",
            _ => return None,
        },
        "vr_chromatic_aberrations" => match id {
            4 => "red",
            5 => "green",
            6 => "blue",
            7 => "falloff",
            8 => "inverse",
            _ => return None,
        },
        "vr_digital_glitch" => match id {
            7 => "distortion",
            24 => "color",
            38 => "seed",
            39 => "amplitude",
            _ => return None,
        },
        "mosaic" => match id {
            1 => "horizontal",
            2 => "vertical",
            3 => "sharp",
            _ => return None,
        },
        "posterize_time" => match id {
            1 => "rate",
            _ => return None,
        },
        "turbulent_displace" => match id {
            1 => "displacement",
            2 => "amount",
            3 => "size",
            4 => "offset",
            5 => "complexity",
            6 => "evolution",
            8 => "cycle",
            9 => "cycle_revs",
            10 => "seed",
            12 => "pinning",
            14 => "antialias",
            _ => return None,
        },
        "roughen_edges" => match id {
            1 => "edge_type",
            2 => "border",
            3 => "sharpness",
            4 => "influence",
            5 => "scale",
            6 => "stretch",
            7 => "offset",
            8 => "complexity",
            9 => "evolution",
            10 => "edge_color",
            14 => "seed",
            _ => return None,
        },
        "linear_wipe_legacy" => match id {
            1 => "completion",
            2 => "angle",
            3 => "feather",
            _ => return None,
        },
        "circle" => match id {
            1 => "center",
            2 => "radius",
            3 => "color",
            _ => return None,
        },
        "opacity" => match id {
            1 => "opacity",
            2 => "blend",
            3 => "",
            _ => return None,
        },
        "ultra_key" => match id {
            1 => "output",
            2 => "setting",
            3 => "key_color",
            4 => "", // Matte Generation section header
            5 => "transparency",
            6 => "highlight",
            7 => "shadow",
            8 => "tolerance",
            9 => "pedestal",
            10 => "",
            11 => "", // Matte Cleanup section header
            12 => "choke",
            13 => "soften",
            14 => "contrast",
            15 => "mid_point",
            16 => "",
            17 => "", // Spill Suppression section header
            18 => "desaturate",
            19 => "range",
            20 => "spill",
            21 => "spill_luma",
            22 => "",
            23 => "", // Color Correction section header
            24 => "cc_saturation",
            25 => "cc_hue",
            26 => "cc_luminance",
            _ => return None,
        },
        "lighting_effects" => {
            // Five lights of eight controls each: type, colour, centre, major/minor radius,
            // angle, intensity, focus. The group headers and hidden separators in between do
            // not exist as FilmCraft parameters.
            for (i, ids) in LIGHT_IDS.iter().enumerate() {
                let base = 2 + i as i32 * 10;
                if id >= base && id < base + 8 {
                    return Some(ids[(id - base) as usize]);
                }
            }
            match id {
                51 => "ambient_color",
                52 => "ambient",
                53 => "gloss",
                54 => "material",
                55 => "exposure",
                58 => "bump_height",
                59 => "white_high",
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(target)
}

/// Localised parameter names and common synonyms. Keys are [`compact`]ed.
fn param_synonyms(effect_id: &str, key: &str) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    let mut add = |ids: &[&'static str]| v.extend_from_slice(ids);
    match key {
        "anchor" | "anchorpoint" | "puntodeanclaje" | "puntoanclaje" | "puntodeancla" => add(&["anchor"]),
        "position" | "posicion" => add(&["position"]),
        "scale" | "escala" => add(&["scale"]),
        "uniformscale" | "escalauniforme" => add(&["uniform_scale"]),
        "scaleheight" | "alturadeescala" | "alturaescala" | "heightscale" => add(&["scale_height"]),
        "scalewidth" | "anchuradeescala" | "anchuraescala" | "widthscale" => add(&["scale_width"]),
        "opacity" | "opacidad" => add(&["opacity"]),
        "rotation" | "rotacion" => add(&["rotation"]),
        "direction" | "direccion" => add(&["direction"]),
        "distance" | "distancia" => add(&["distance"]),
        "softness" | "suavizado" | "suavizacion" | "smoothness" => add(&["softness", "smoothness"]),
        "blurriness" | "desenfoque" | "desenfoquegaussiano" | "blur" | "radius" if effect_id == "gaussian_blur" => {
            add(&["blurriness", "radius"])
        }
        "dimensions" | "dimensiones" | "dimensionesdedesenfoque" | "blurdimensions" => add(&["dimensions"]),
        "repeatedge" | "repeatedgepixels" | "repetirbordes" => add(&["repeat_edge"]),
        "amount" | "cantidad" | "nivel" | "niveldecantidad" | "tintamount" | "intensity" => add(&["amount", "intensity"]),
        "threshold" | "umbral" => add(&["threshold"]),
        "radius" | "radio" => add(&["radius"]),
        "brightness" | "brillo" => add(&["brightness"]),
        "saturation" | "saturacion" => add(&["saturation"]),
        "tint" | "tinte" => add(&["tint", "color"]),
        "usetint" | "usarcolor" | "usarcolordetincion" => add(&["use_tint"]),
        "color" | "colour" | "colorde" | "colordesombra" | "sombracolor" => add(&["color", "key_color"]),
        "height" | "alto" | "altura" | "alturadeonda" => add(&["height"]),
        "width" | "ancho" | "anchura" | "anchuradeonda" => add(&["width"]),
        "speed" | "velocidad" | "velocidaddeonda" => add(&["speed"]),
        "complexity" | "complejidad" => add(&["complexity"]),
        "evolution" | "evolucion" => add(&["evolution"]),
        "seed" | "semilla" => add(&["seed"]),
        "left" | "izquierda" => add(&["left"]),
        "top" | "arriba" => add(&["top"]),
        "right" | "derecha" => add(&["right"]),
        "bottom" | "abajo" => add(&["bottom"]),
        "zoom" => add(&["zoom"]),
        "completion" | "finalizacion" | "transicion" => add(&["completion"]),
        "feather" | "pluma" | "suavizadodeborde" => add(&["feather"]),
        "center" | "centro" | "centrodereseccion" => add(&["center"]),
        "angle" | "angulo" => add(&["angle"]),
        "keycolor" | "colordeclave" => add(&["key_color"]),
        "output" | "salida" => add(&["output"]),
        "setting" | "ajuste" => add(&["setting"]),
        "transparency" | "transparencia" => add(&["transparency"]),
        "highlight" | "iluminacion" | "brillodealtasluces" => add(&["highlight"]),
        "shadow" | "sombra" => add(&["shadow"]),
        "tolerance" | "tolerancia" => add(&["tolerance"]),
        "pedestal" => add(&["pedestal"]),
        "gain" | "ganancia" => add(&["gain", "level"]),
        "frequency" | "frecuencia" => add(&["frequency", "cutoff"]),
        "cutoff" | "cortedefrecuencia" | "frecuenciadecorte" => add(&["cutoff", "frequency"]),
        "ratio" | "relacion" => add(&["ratio"]),
        "attack" | "ataque" => add(&["attack"]),
        "release" | "liberacion" | "caida" => add(&["release"]),
        "hold" | "mantener" => add(&["hold"]),
        "boost" | "amplificacion" | "refuerzo" => add(&["boost"]),
        "balance" | "equilibrio" => add(&["balance"]),
        "mix" | "mezcla" => add(&["mix", "amount"]),
        "bypass" | "omitir" => add(&["bypass"]),
        "level" | "niveldevolumen" => add(&["level"]),
        _ => {}
    }
    v
}

// ---------------------------------------------------------------- values

/// Parse and convert a raw parameter into a typed FilmCraft `Param`.
fn parse_param(pdef: &ParamDef, effect_id: &str, premiere_id: i32, raw: &RawParam, anchor_in: i64) -> Option<Param> {
    let static_text = raw.static_value();
    let raw_keys = raw.keyframes.as_deref().map(raw_keyframes).unwrap_or_default();
    match &pdef.kind {
        ParamKind::Float { min, max, .. } => {
            let base = static_text.and_then(parse_f64).unwrap_or_else(|| pdef.default.as_f64().unwrap_or(0.0));
            let base = convert_float(effect_id, premiere_id, base);
            let mut keys = Vec::new();
            if pdef.animatable {
                for k in &raw_keys {
                    let Some(value) = parse_f64(&k.value) else { continue };
                    keys.push(Keyframe {
                        time: Tick(relative_time(k.time, anchor_in)),
                        value: ParamValue::Float(clamp_float(convert_float(effect_id, premiere_id, value), *min, *max)),
                        interp: k.interp,
                        in_influence: k.in_influence,
                        out_influence: k.out_influence,
                    });
                }
            }
            finish_param(ParamValue::Float(clamp_float(base, *min, *max)), keys)
        }
        ParamKind::Angle => {
            let base = static_text.and_then(parse_f64).unwrap_or_else(|| pdef.default.as_f64().unwrap_or(0.0));
            let mut keys = Vec::new();
            if pdef.animatable {
                for k in &raw_keys {
                    let Some(value) = parse_f64(&k.value) else { continue };
                    keys.push(Keyframe {
                        time: Tick(relative_time(k.time, anchor_in)),
                        value: ParamValue::Float(value),
                        interp: k.interp,
                        in_influence: k.in_influence,
                        out_influence: k.out_influence,
                    });
                }
            }
            finish_param(ParamValue::Float(base), keys)
        }
        ParamKind::Point => {
            let base = static_text.and_then(parse_point).or_else(|| pdef.default.as_vec2()).unwrap_or(Vec2::new(960.0, 540.0));
            let mut keys = Vec::new();
            if pdef.animatable {
                for k in &raw_keys {
                    let Some(value) = parse_point(&k.value) else { continue };
                    keys.push(Keyframe {
                        time: Tick(relative_time(k.time, anchor_in)),
                        value: ParamValue::Vec2(value),
                        interp: k.interp,
                        in_influence: k.in_influence,
                        out_influence: k.out_influence,
                    });
                }
            }
            finish_param(ParamValue::Vec2(base), keys)
        }
        ParamKind::Color => {
            let base = static_text.and_then(parse_color).or_else(|| pdef.default.as_color()).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let mut keys = Vec::new();
            if pdef.animatable {
                for k in &raw_keys {
                    let Some(value) = parse_color(&k.value) else { continue };
                    keys.push(Keyframe {
                        time: Tick(relative_time(k.time, anchor_in)),
                        value: ParamValue::Color(value),
                        interp: k.interp,
                        in_influence: k.in_influence,
                        out_influence: k.out_influence,
                    });
                }
            }
            finish_param(ParamValue::Color(base), keys)
        }
        ParamKind::Bool => {
            let base = static_text.map(parse_bool).or_else(|| pdef.default.as_bool()).unwrap_or(false);
            // Premiere stores "Use Composition's Shutter Angle"; FilmCraft stores the opposite.
            let base = if effect_id == "transform" && premiere_id == 9 { !base } else { base };
            Some(Param::new(ParamValue::Bool(base)))
        }
        ParamKind::Choice(options) => {
            let base = static_text.and_then(parse_f64).unwrap_or_else(|| pdef.default.as_f64().unwrap_or(0.0));
            let mut choice = choice_index(effect_id, pdef.id, base);
            if options.is_empty() {
                choice = 0;
            } else if choice as usize >= options.len() {
                choice = 0;
            }
            Some(Param::new(ParamValue::Choice(choice)))
        }
        _ => None,
    }
}

/// Sort, deduplicate, and assemble keyframes into a finalized parameter.
fn finish_param(mut base: ParamValue, mut keys: Vec<Keyframe>) -> Option<Param> {
    if keys.is_empty() {
        return Some(Param::new(base));
    }
    keys.sort_by_key(|k| k.time);
    keys.dedup_by_key(|k| k.time);
    if let Some(first) = keys.first() {
        base = first.value.clone();
    }
    Some(Param { value: base, keyframes: keys })
}

/// Unit conversions between Premiere's stored ranges and FilmCraft's.
fn convert_float(effect_id: &str, premiere_id: i32, value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    if effect_id == "drop_shadow" && premiere_id == 2 {
        // Premiere stores Drop Shadow opacity as 0..255 even though it shows 0..100 %.
        return (value * 100.0 / 255.0).clamp(0.0, 100.0);
    }
    if effect_id == "vr_glow" && premiere_id == 2 {
        // VR Glow shows the luminance threshold as 0..1; FilmCraft stores it as a percentage.
        return (value * 100.0).clamp(0.0, 100.0);
    }
    value
}

/// Convert a numeric choice value to a zero-based option index.
fn choice_index(effect_id: &str, param: &str, value: f64) -> u32 {
    if !value.is_finite() || value < 0.0 {
        return 0;
    }
    let n = value as u32;
    // Premiere's opacity blend-mode enum numbers Normal as 18; the rest of the list is not
    // guaranteed to match FilmCraft's, so only the common default is translated.
    if effect_id == "opacity" && param == "blend" && n == 18 {
        return 0;
    }
    n
}

/// Clamp a floating-point value to the specified range, handling non-finite numbers.
fn clamp_float(v: f64, min: f64, max: f64) -> f64 {
    if !v.is_finite() {
        return min;
    }
    if max >= min { v.clamp(min, max) } else { min }
}

struct RawKey {
    time: i64,
    value: String,
    interp: Interpolation,
    in_influence: f64,
    out_influence: f64,
}

/// Parse raw semicolon/comma delimited keyframe text into structured keys.
fn raw_keyframes(text: &str) -> Vec<RawKey> {
    let mut out = Vec::new();
    for entry in text.split(';') {
        if out.len() >= MAX_KEYFRAMES {
            break;
        }
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split(',').collect();
        if parts.len() < 2 {
            continue;
        }
        let Some(time) = parts[0].trim().parse::<i64>().ok() else { continue };
        out.push(RawKey {
            time,
            value: parts[1].trim().to_string(),
            interp: parse_interp(parts.get(2).copied()),
            in_influence: parts.get(5).and_then(|s| s.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(1.0 / 3.0),
            out_influence: parts.get(7).and_then(|s| s.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(1.0 / 3.0),
        });
    }
    out
}

/// `StartKeyframe` is `time,value,interp,…`; the first field is a sentinel for static values.
fn start_keyframe_value(text: &str) -> Option<&str> {
    text.split(',').nth(1).map(str::trim).filter(|s| !s.is_empty())
}

/// Calculate relative keyframe time against an anchor point, clamped to Tick bounds.
fn relative_time(time: i64, anchor_in: i64) -> i64 {
    (time as i128 - anchor_in as i128).clamp(Tick::MIN.0 as i128, Tick::MAX.0 as i128) as i64
}

/// Parse a 2D point string into a Vec2 in pixel space.
fn parse_point(text: &str) -> Option<Vec2> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let (x, y) = raw.split_once(':').or_else(|| raw.split_once(','))?;
    let x = x.trim().parse::<f64>().ok()?;
    let y = y.trim().parse::<f64>().ok()?;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    // Premiere stores geometry as normalised 0..1 coordinates. A few effects already use pixels.
    if (-2.0..=2.0).contains(&x) && (-2.0..=2.0).contains(&y) {
        Some(Vec2::new(x * 1920.0, y * 1080.0))
    } else {
        Some(Vec2::new(x, y))
    }
}

/// Parse a Premiere 64-bit color integer into RGBA float components.
fn parse_color(text: &str) -> Option<[f32; 4]> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "0" {
        return Some([0.0, 0.0, 0.0, 1.0]);
    }
    let n = trimmed.parse::<u64>().ok()?;
    let mut a = ((n >> 48) & 0xffff) as f32 / 65280.0;
    if a <= 1e-4 {
        a = 1.0;
    }
    let r = ((n >> 32) & 0xffff) as f32 / 65280.0;
    let g = ((n >> 16) & 0xffff) as f32 / 65280.0;
    let b = (n & 0xffff) as f32 / 65280.0;
    Some([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), a.clamp(0.0, 1.0)])
}

/// Parse a boolean string from common textual representations.
fn parse_bool(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

/// Parse a floating-point number from text, ensuring finite values.
fn parse_f64(text: &str) -> Option<f64> {
    let t = text.trim().trim_end_matches('.');
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse a 64-bit signed integer from text.
fn parse_i64(text: &str) -> Option<i64> {
    text.trim().parse::<i64>().ok()
}

/// Parse an unsigned size integer from text.
fn parse_usize(text: &str) -> Option<usize> {
    text.trim().parse::<usize>().ok()
}

/// Map Premiere keyframe interpolation codes to FilmCraft `Interpolation` enum.
fn parse_interp(code: Option<&str>) -> Interpolation {
    match code.map(str::trim) {
        Some("0") => Interpolation::Linear,
        Some("1") => Interpolation::Hold,
        Some("4") => Interpolation::ContinuousBezier,
        Some("5") => Interpolation::Bezier,
        Some("7") => Interpolation::AutoBezier,
        _ => Interpolation::Linear,
    }
}

// ---------------------------------------------------------------- helpers

/// Extract trimmed text from a child element matching the given tag.
fn child_text(node: Node<'_, '_>, tag: &str) -> Option<String> {
    node.children()
        .find(|n| n.tag_name().name() == tag)
        .and_then(|n| n.text())
        .map(|s| s.to_string())
}

/// Lower-case alphanumeric key (no spaces, punctuation or accents), used for names.
fn compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// Sanitize and clamp preset names, removing control characters.
fn sanitize_name(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim();
    if trimmed.chars().count() > 200 {
        trimmed.chars().take(200).collect::<String>().trim_end().to_string()
    } else {
        trimmed.to_string()
    }
}

use filmcraft_geom::Vec2;
use filmcraft_project::{EffectInstance, Interpolation, Keyframe, Param, ParamValue, find_effect};
use filmcraft_time::Tick;
use roxmltree::Node;

use super::graph::{Graph, error, static_text, text_at};
use crate::xml::child_text;
use crate::{Report, Result};

#[derive(Clone, Copy)]
enum Conversion {
    Native,
    Point((u32, u32)),
    Gain,
}

/// Coordinates are normalised in native files. Motion's position is in sequence pixels;
/// anchors and standard effects are in source pixels.
pub(super) fn component(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    origin: Tick,
    sequence_size: (u32, u32),
    source_size: (u32, u32),
    report: &mut Report,
) -> Result<Option<EffectInstance>> {
    let audio = node.has_tag_name("AudioFilterComponent");
    let match_name = child_text(node, if audio { "FilterMatchName" } else { "MatchName" }).unwrap_or("");
    let id = match match_name {
        "AE.ADBE Motion" => "motion",
        "AE.ADBE Geometry2" => "transform",
        "AE.ADBE Opacity" => "opacity",
        "AE.ADBE AECrop" => "crop",
        "AE.ADBE Offset" => "offset",
        "AE.ADBE Motion Blur" => "directional_blur",
        "AE.ADBE Gaussian Blur 2" => "gaussian_blur",
        "Internal Volume Mono" | "Internal Volume Stereo" => "volume",
        _ => {
            let name = text_at(node, &["Component", "DisplayName"]).or_else(|| child_text(node, "CustomDisplayName")).unwrap_or(match_name);
            report.warn(format!("Premiere effect \"{name}\" ({match_name}) is not supported and was skipped"));
            return Ok(None);
        }
    };
    let Some(def) = find_effect(id) else { return Ok(None) };
    let mut effect = def.instance();
    let component_path: &[&str] = if audio { &["AudioComponent", "Component"] } else { &["Component"] };
    effect.enabled = !graph.boolean(node, &component_path.iter().copied().chain(["Bypass"]).collect::<Vec<_>>(), false)?;
    let params_path: Vec<&str> = component_path.iter().copied().chain(["Params"]).collect();
    let parameters = graph.references(node, &params_path)?;
    let mut composition_shutter = false;
    let mut volume_muted = false;
    for (index, native) in parameters.into_iter().enumerate() {
        let number = graph.integer(native, &["ParameterID"], (index + 1) as i64)?;
        let mapped = match (id, number) {
            ("motion", 1) => Some(("position", Conversion::Point(sequence_size))),
            ("motion", 2) => Some(("scale", Conversion::Native)),
            ("motion", 3) => Some(("scale_width", Conversion::Native)),
            ("motion", 4) => Some(("uniform_scale", Conversion::Native)),
            ("motion", 5) => Some(("rotation", Conversion::Native)),
            ("motion", 6) => Some(("anchor", Conversion::Point(source_size))),
            ("motion", 7) => Some(("anti_flicker", Conversion::Native)),
            ("transform", 1) => Some(("anchor", Conversion::Point(source_size))),
            ("transform", 2) => Some(("position", Conversion::Point(source_size))),
            ("transform", 3) => Some(("scale_height", Conversion::Native)),
            ("transform", 4) => Some(("scale_width", Conversion::Native)),
            ("transform", 5) => Some(("skew", Conversion::Native)),
            ("transform", 6) => Some(("skew_axis", Conversion::Native)),
            ("transform", 7) => Some(("rotation", Conversion::Native)),
            ("transform", 8) | ("opacity", 1) => Some(("opacity", Conversion::Native)),
            ("transform", 10) => Some(("shutter_angle", Conversion::Native)),
            ("transform", 11) => Some(("uniform_scale", Conversion::Native)),
            ("crop", 1) => Some(("left", Conversion::Native)),
            ("crop", 2) => Some(("top", Conversion::Native)),
            ("crop", 3) => Some(("right", Conversion::Native)),
            ("crop", 4) => Some(("bottom", Conversion::Native)),
            ("crop", 5) => Some(("zoom", Conversion::Native)),
            ("crop", 6) => Some(("feather", Conversion::Native)),
            ("offset", 1) => Some(("shift", Conversion::Point(source_size))),
            ("offset", 2) => Some(("blend", Conversion::Native)),
            ("directional_blur", 1) => Some(("direction", Conversion::Native)),
            ("directional_blur", 2) => Some(("length", Conversion::Native)),
            ("gaussian_blur", 1) => Some(("blurriness", Conversion::Native)),
            ("gaussian_blur", 3) => Some(("repeat_edge", Conversion::Native)),
            ("volume", 2) => Some(("level", Conversion::Gain)),
            _ => None,
        };
        if let Some((target, conversion)) = mapped {
            let Some(default) = effect.params.get(target).map(|p| p.value.clone()) else { continue };
            effect.params.insert(target.to_string(), parameter(graph, native, origin, &default, conversion, match_name, report)?);
        } else if id == "transform" && number == 9 {
            composition_shutter = matches!(static_text(native), Some("true" | "1"));
        } else if id == "volume" && number == 1 {
            volume_muted = matches!(static_text(native), Some("true" | "1"));
            if graph.boolean(native, &["IsTimeVarying"], false)? {
                report.warn("Premiere animated Volume mute is not supported; its static state was used");
            }
        } else {
            // These are known neutral legacy controls, not unknown active effects.
            let neutral = !graph.boolean(native, &["IsTimeVarying"], false)?
                && ((id == "motion" && (8..=11).contains(&number) && static_text(native).is_none_or(|v| v.parse::<f64>().ok() == Some(0.0)))
                    || (id == "transform" && number == 12 && static_text(native) == Some("0"))
                    || (id == "opacity" && ((number == 2 && static_text(native) == Some("18")) || (number == 3 && static_text(native) == Some("0")))));
            if !neutral {
                report.warn(format!("Premiere effect {match_name} parameter {number} is not supported; the FilmCraft default was used"));
            }
        }
    }
    if composition_shutter {
        effect.params.insert("shutter_angle".into(), Param::new(ParamValue::Float(180.0)));
        report.warn("Premiere Transform uses the composition shutter angle; imported as 180 degrees");
    }
    if volume_muted {
        effect.params.insert("level".into(), Param::new(ParamValue::Float(-287.5)));
    }
    if node.descendants().any(|n| n.is_element() && matches!(n.tag_name().name(), "Mask" | "Masks" | "MaskComponents")) {
        report.warn(format!("Premiere effect {match_name} masks are not supported and were skipped"));
    }
    Ok(Some(effect))
}

fn finite(text: &str, graph: &Graph<'_, '_>) -> Result<f64> {
    let v = text.trim().parse::<f64>().map_err(|e| error(graph.format, format!("invalid effect value: {e}")))?;
    if !v.is_finite() || v.abs() > 1e12 {
        return Err(error(graph.format, "effect value is not finite or is out of range"));
    }
    Ok(v)
}

fn value(text: &str, default: &ParamValue, conversion: Conversion, graph: &Graph<'_, '_>) -> Result<ParamValue> {
    match conversion {
        Conversion::Point((w, h)) => {
            let (x, y) = text.split_once(':').ok_or_else(|| error(graph.format, "point parameter must contain x:y"))?;
            let x = finite(x, graph)? * f64::from(w);
            let y = finite(y, graph)? * f64::from(h);
            if x.abs() > 1e12 || y.abs() > 1e12 {
                return Err(error(graph.format, "point parameter is out of range"));
            }
            Ok(ParamValue::Vec2(Vec2::new(x, y)))
        }
        Conversion::Gain => {
            let gain = finite(text, graph)?;
            if gain < 0.0 {
                return Err(error(graph.format, "negative audio gain"));
            }
            Ok(ParamValue::Float(if gain == 0.0 { -287.5 } else { (20.0 * gain.log10()).max(-287.5) }))
        }
        Conversion::Native => match default {
            ParamValue::Bool(_) => match text {
                "true" | "1" => Ok(ParamValue::Bool(true)),
                "false" | "0" => Ok(ParamValue::Bool(false)),
                _ => Err(error(graph.format, "invalid effect boolean")),
            },
            ParamValue::Choice(_) => text.parse::<u32>().map(ParamValue::Choice).map_err(|e| error(graph.format, e.to_string())),
            _ => Ok(ParamValue::Float(finite(text, graph)?)),
        },
    }
}

fn parameter(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    origin: Tick,
    default: &ParamValue,
    conversion: Conversion,
    match_name: &str,
    report: &mut Report,
) -> Result<Param> {
    let mut p = Param::new(static_text(node).map(|v| value(v, default, conversion, graph)).transpose()?.unwrap_or_else(|| default.clone()));
    if !graph.boolean(node, &["IsTimeVarying"], false)? {
        return Ok(p);
    }
    let hold = graph.boolean(node, &["DiscontinuousInterpolate"], false)?;
    let mut approximated = false;
    for record in child_text(node, "Keyframes").unwrap_or("").split(';').filter(|r| !r.trim().is_empty()) {
        graph.charge()?;
        if p.keyframes.len() >= 65_536 {
            return Err(error(graph.format, "parameter exceeds the 65536 keyframe limit"));
        }
        let fields: Vec<&str> = record.split(',').take(65).map(str::trim).collect();
        if fields.len() > 64 {
            return Err(error(graph.format, "keyframe has too many fields"));
        }
        let timestamp =
            fields.first().ok_or_else(|| error(graph.format, "keyframe time is missing"))?.parse::<i64>().map_err(|e| error(graph.format, e.to_string()))?;
        let time = timestamp.checked_sub(origin.0).ok_or_else(|| error(graph.format, "keyframe time overflows"))?;
        if time.unsigned_abs() > super::MAX_TIME_TICKS as u64 {
            return Err(error(graph.format, "keyframe time is out of range"));
        }
        let data = fields.get(1).ok_or_else(|| error(graph.format, "keyframe value is missing"))?;
        let mut key = Keyframe::new(Tick(time), value(data, default, conversion, graph)?);
        let incoming = fields.get(5).map(|v| finite(v, graph)).transpose()?.unwrap_or(0.0);
        let outgoing = fields.get(7).map(|v| finite(v, graph)).transpose()?.unwrap_or(0.0);
        let in_speed = fields.get(4).map(|v| finite(v, graph)).transpose()?.unwrap_or(0.0);
        let out_speed = fields.get(6).map(|v| finite(v, graph)).transpose()?.unwrap_or(0.0);
        if !(0.0..=1.0).contains(&incoming) || !(0.0..=1.0).contains(&outgoing) {
            return Err(error(graph.format, "keyframe influence is outside 0..1"));
        }
        key.in_influence = incoming;
        key.out_influence = outgoing;
        key.interp = if hold || !key.value.interpolates() {
            Interpolation::Hold
        } else if incoming > 0.0 || outgoing > 0.0 {
            Interpolation::Bezier
        } else {
            Interpolation::Linear
        };
        approximated |= in_speed != 0.0 || out_speed != 0.0 || fields.get(10..).is_some_and(|v| v.iter().any(|s| s.parse::<f64>().is_ok_and(|v| v != 0.0)));
        p.keyframes.push(key);
    }
    p.keyframes.sort_by_key(|k| k.time);
    if p.keyframes.windows(2).any(|k| k[0].time == k[1].time) {
        return Err(error(graph.format, "duplicate parameter keyframe time"));
    }
    if approximated {
        report.warn(format!("Premiere effect {match_name} temporal velocities or spatial tangents were approximated by FilmCraft keyframe interpolation"));
    }
    Ok(p)
}

pub(super) fn put_effect(effects: &mut Vec<EffectInstance>, effect: EffectInstance) {
    if effect.def().is_some_and(|e| e.intrinsic) {
        if let Some(old) = effects.iter_mut().find(|e| e.effect == effect.effect) {
            *old = effect;
        } else {
            effects.push(effect);
        }
    } else {
        let first_intrinsic = effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(effects.len());
        effects.insert(first_intrinsic, effect);
    }
}

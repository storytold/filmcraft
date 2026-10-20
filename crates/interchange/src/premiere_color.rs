//! Bounded scalar Lumetri mapping observed in contributor-owned project XML.
//! Existing PR #1694 informed matching effects to FilmCraft's registry, but its
//! optional-control-type/name matching cannot map these files. Stable IDs avoid
//! duplicate labels and work when ParameterControlType is omitted. Original work.
use super::{error, referred, spend, static_parameter_value, value, warning};
use crate::xml::child_text;
use crate::{Report, Result};
use filmcraft_project::{EffectInstance, Keyframe, Param, ParamKind, ParamValue, find_effect};
use filmcraft_time::Tick;
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};

fn mapped(id: &str) -> Option<&'static str> {
    Some(match id {
        "3" => "basic_on",
        "7" => "temperature",
        "8" => "tint",
        "11" => "exposure",
        "12" => "contrast",
        "13" => "highlights",
        "14" => "shadows",
        "15" => "whites",
        "16" => "blacks",
        "20" => "saturation",
        "23" => "creative_on",
        "26" => "look_intensity",
        "28" => "faded_film",
        "29" => "sharpen",
        "30" => "vibrance",
        "31" => "creative_sat",
        "37" => "curves_on",
        "46" => "wheels_on",
        "50" => "vignette_on",
        "51" => "vignette_amount",
        "52" => "vignette_midpoint",
        "53" => "vignette_roundness",
        "54" => "vignette_feather",
        _ => return None,
    })
}

fn convert(raw: &str, kind: &ParamKind, report: &mut Report) -> Result<ParamValue> {
    match kind {
        ParamKind::Bool => match raw.trim() {
            "true" | "1" => Ok(ParamValue::Bool(true)),
            "false" | "0" => Ok(ParamValue::Bool(false)),
            _ => Err(error("invalid color switch")),
        },
        ParamKind::Float { min, max, .. } => {
            let v: f64 = raw.trim().parse().map_err(|_| error("invalid color scalar"))?;
            if !v.is_finite() {
                return Err(error("non-finite color scalar"));
            }
            if v < *min || v > *max {
                warning(report, "Lumetri scalar exceeds FilmCraft's supported range; clamped");
            }
            Ok(ParamValue::Float(v.clamp(*min, *max)))
        }
        _ => Err(error("unsupported color parameter kind")),
    }
}

pub(super) fn read<'a, 'i>(component: Node<'a, 'i>, ids: &BTreeMap<&str, Node<'a, 'i>>, work: &mut usize, report: &mut Report) -> Result<EffectInstance> {
    let def = find_effect("lumetri").ok_or_else(|| error("Lumetri effect unavailable"))?;
    let mut effect = def.instance();
    effect.enabled = !matches!(value(component, "Bypass"), Some("true" | "1"));
    let mut seen = BTreeSet::new();
    for pointer in component.descendants().filter(|n| n.has_tag_name("Param")) {
        let Some(p) = referred(Some(pointer), ids, "ObjectRef") else { continue };
        spend(work, p)?;
        let Some(key) = child_text(p, "ParameterID").and_then(mapped) else { continue };
        if !seen.insert(key) {
            return Err(error("duplicate mapped color parameter"));
        }
        let Some(pd) = def.params.iter().find(|p| p.id == key) else { continue };
        let Some(raw) = static_parameter_value(p) else { continue };
        let mut param = Param::new(convert(raw, &pd.kind, report)?);
        if let Some(keys) = child_text(p, "Keyframes") {
            if matches!(pd.kind, ParamKind::Bool) {
                warning(report, "animated Lumetri section switches unsupported; static value used");
            } else {
                for entry in keys.split(';').filter(|e| !e.trim().is_empty()) {
                    if param.keyframes.len() >= 10000 {
                        return Err(error("too many color keyframes"));
                    }
                    let mut fields = entry.split(',');
                    let tick: i64 =
                        fields.next().ok_or_else(|| error("color keyframe missing time"))?.trim().parse().map_err(|_| error("invalid color keyframe time"))?;
                    if !(0..=Tick::MAX.0).contains(&tick) {
                        return Err(error("color keyframe outside tick bounds"));
                    }
                    let value = convert(fields.next().ok_or_else(|| error("color keyframe missing value"))?, &pd.kind, report)?;
                    param.keyframes.push(Keyframe::new(Tick(tick), value));
                }
                param.keyframes.sort_by_key(|k| k.time);
                if param.keyframes.windows(2).any(|k| k[0].time == k[1].time) {
                    return Err(error("duplicate color keyframe time"));
                }
                if !param.keyframes.is_empty() {
                    warning(report, "Lumetri keyframe values/times imported; interpolation approximated with linear segments");
                }
            }
        }
        effect.params.insert(key.into(), param);
    }
    warning(
        report,
        "Lumetri scalar controls imported; opaque LUTs, curves, wheels and HSL data retained in provenance, not imported; renderer parity is not guaranteed",
    );
    Ok(effect)
}

//! Bounded native Motion animation controls, observed in contributor-owned XML.
//! Exact source-time control points; spatial/temporal easing remains approximate.
use super::{error, static_parameter_value, warning};
use crate::xml::child_text;
use crate::{Report, Result};
use filmcraft_geom::Vec2;
use filmcraft_project::{Interpolation, Keyframe, Param, ParamValue};
use filmcraft_time::Tick;
use roxmltree::Node;

fn number(raw: &str) -> Result<f64> {
    let n: f64 = raw.trim().parse().map_err(|_| error("invalid animated Motion number"))?;
    if !n.is_finite() || n.abs() > 1_000_000.0 {
        return Err(error("animated Motion number outside bounds"));
    }
    Ok(n)
}

fn value(raw: &str, key: &str) -> Result<ParamValue> {
    match key {
        "Position" => {
            let (x, y) = raw.split_once(':').ok_or_else(|| error("animated Motion point requires two coordinates"))?;
            Ok(ParamValue::Vec2(Vec2::new(number(x)?, number(y)?)))
        }
        "Scale" => {
            let n = number(raw)?;
            if !(0.0..=10000.0).contains(&n) {
                return Err(error("animated Motion scale outside bounds"));
            }
            Ok(ParamValue::Float(n))
        }
        "Rotation" => Ok(ParamValue::Float(number(raw)?)),
        _ => Err(error("unsupported animated Motion control")),
    }
}

pub(super) fn read(p: Node<'_, '_>, key: &str, report: &mut Report) -> Result<Param> {
    let raw = static_parameter_value(p).ok_or_else(|| error("animated Motion missing base value"))?;
    let mut param = Param::new(value(raw, key)?);
    let hold = child_text(p, "DiscontinuousInterpolate").is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    for entry in child_text(p, "Keyframes").unwrap_or("").split(';').filter(|s| !s.trim().is_empty()) {
        if param.keyframes.len() >= 10000 {
            return Err(error("too many Motion keyframes"));
        }
        let mut fields = entry.split(',');
        let time: i64 =
            fields.next().ok_or_else(|| error("Motion keyframe missing time"))?.trim().parse().map_err(|_| error("invalid Motion keyframe time"))?;
        if !(0..=Tick::MAX.0).contains(&time) {
            return Err(error("Motion keyframe outside tick bounds"));
        }
        let mut k = Keyframe::new(Tick(time), value(fields.next().ok_or_else(|| error("Motion keyframe missing value"))?, key)?);
        k.interp = if hold { Interpolation::Hold } else { Interpolation::Linear };
        param.keyframes.push(k);
    }
    param.keyframes.sort_by_key(|k| k.time);
    if param.keyframes.windows(2).any(|k| k[0].time == k[1].time) {
        return Err(error("duplicate Motion keyframe time"));
    }
    if param.keyframes.is_empty() {
        warning(report, "animated Motion keyframe data absent; static value used");
    } else {
        warning(report, "animated Motion keyframe values and source times imported; spatial/temporal easing approximated by linear or hold segments");
    }
    Ok(param)
}

/// Normalised positions become sequence pixels, including each animated control point.
pub(super) fn in_pixels(mut param: Param, width: u32, height: u32) -> Param {
    param.map_values(|value| match value {
        ParamValue::Vec2(p) => ParamValue::Vec2(Vec2::new(p.x * f64::from(width), p.y * f64::from(height))),
        other => other,
    });
    param
}

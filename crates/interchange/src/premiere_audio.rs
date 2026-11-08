//! Intrinsic audio parameter mapping from contributor-authored project XML.
//! Raw normalized 0.17782794 reads as 0 dB in Premiere; zero represents silence.
//! Original work, MIT OR Apache-2.0. Temporal handles are explicitly approximate.
use super::{error, referred, spend, value, warning};
use crate::xml::child;
use crate::{Report, Result};
use filmcraft_project::{Keyframe, Param, ParamValue};
use filmcraft_time::Tick;
use roxmltree::Node;
use std::collections::BTreeMap;

fn level(raw: &str) -> Result<ParamValue> {
    let n: f64 = raw.trim().parse().map_err(|_| error("invalid intrinsic audio level"))?;
    if !n.is_finite() || !(0.0..=1.0).contains(&n) {
        return Err(error("intrinsic audio level outside normalized bounds"));
    }
    Ok(ParamValue::Float(if n == 0.0 { -287.5 } else { (20.0 * n.log10() + 15.0).max(-287.5) }))
}

/// `match_name` is the caller's (`MatchName`, else `FilterMatchName`), so both agree on which
/// control this component is.
pub(super) fn read<'a, 'i>(
    component: Node<'a, 'i>,
    match_name: &str,
    ids: &BTreeMap<&str, Node<'a, 'i>>,
    work: &mut usize,
    out: &mut BTreeMap<String, Param>,
    report: &mut Report,
) -> Result<()> {
    // Parameters are matched by their `Param@Index`: `Name` is localised (a Spanish Premiere
    // writes Nivel / Izquierda / Derecha) while the index layout is not. Volume is Mute (0),
    // Level (1); Channel Volume is Bypass (0), Left (1), Right (2).
    let volume = match_name.starts_with("Internal Volume");
    for ptr in component.descendants().filter(|n| n.has_tag_name("Param")) {
        let Some(p) = referred(Some(ptr), ids, "ObjectRef") else { continue };
        spend(work, p)?;
        let key = match (volume, ptr.attribute("Index")) {
            (true, Some("1")) => "volume.level",
            (false, Some("1")) => "channel_volume.left",
            (false, Some("2")) => "channel_volume.right",
            (_, Some("0")) => {
                if value(p, "CurrentValue").is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")) || value(p, "Keyframes").is_some() {
                    warning(report, "intrinsic audio mute/bypass automation retained in provenance, not imported");
                }
                continue;
            }
            _ => continue,
        };
        let Some(raw) = value(p, "CurrentValue").or_else(|| value(p, "StartKeyframe").and_then(|v| v.split(',').nth(1))) else { continue };
        let mut param = Param::new(level(raw)?);
        if let Some(keys) = value(p, "Keyframes") {
            for entry in keys.split(';').filter(|s| !s.trim().is_empty()) {
                if param.keyframes.len() >= 10000 {
                    return Err(error("too many intrinsic audio keyframes"));
                }
                let mut fields = entry.split(',');
                let time: i64 =
                    fields.next().ok_or_else(|| error("audio keyframe without time"))?.trim().parse().map_err(|_| error("invalid audio keyframe tick"))?;
                if !(0..=Tick::MAX.0).contains(&time) {
                    return Err(error("audio keyframe outside tick bounds"));
                }
                let val = level(fields.next().ok_or_else(|| error("audio keyframe without value"))?)?;
                param.keyframes.push(Keyframe::new(Tick(time), val));
            }
            // FilmCraft cannot express Premiere's native amplitude/ease handles exactly.
            // Keep exact source-time control points and warn rather than imply sound parity.
            if !param.keyframes.is_empty() {
                warning(report, "intrinsic audio keyframe values/times imported; interpolation approximated with linear dB segments");
            }
            param.keyframes.sort_by_key(|k| k.time);
            if param.keyframes.windows(2).any(|p| p[0].time == p[1].time) {
                return Err(error("duplicate intrinsic audio keyframe time"));
            }
        }
        out.insert(key.to_string(), param);
    }
    Ok(())
}

fn is_true(v: &str) -> bool {
    v == "1" || v.eq_ignore_ascii_case("true")
}

/// Whether an audio track is muted on its mixer fader: `AudioTrack/ComponentOwner` points to the
/// track's component chain, whose `AudioFader` holds Volume (`Param@Index` 0) and Mute (1). Read
/// by index because the parameter `Name` is localised. Mute automation is not imported: the
/// value at the start of the sequence is used and a warning says so.
pub(super) fn fader_muted<'a, 'i>(
    track: Node<'a, 'i>,
    ids: &BTreeMap<&str, Node<'a, 'i>>,
    work: &mut usize,
    report: &mut Report,
    context: &str,
) -> Result<bool> {
    let owner = child(track, "AudioTrack").and_then(|t| child(t, "ComponentOwner")).and_then(|o| child(o, "Components"));
    let Some(chain) = referred(owner, ids, "ObjectRef") else { return Ok(false) };
    spend(work, chain)?;
    let components = child(chain, "ComponentChain").and_then(|c| child(c, "Components")).or_else(|| child(chain, "Components"));
    let fader = components
        .into_iter()
        .flat_map(|c| c.children())
        .filter(|c| c.has_tag_name("Component"))
        .filter_map(|c| referred(Some(c), ids, "ObjectRef"))
        .find(|c| c.has_tag_name("AudioFader"));
    let Some(fader) = fader else { return Ok(false) };
    spend(work, fader)?;
    let pointer = fader.descendants().find(|n| n.has_tag_name("Param") && n.attribute("Index") == Some("1"));
    let Some(mute) = referred(pointer, ids, "ObjectRef") else { return Ok(false) };
    spend(work, mute)?;
    let start = value(mute, "StartKeyframe").and_then(|k| k.split(',').nth(1)).map(str::trim);
    let muted = value(mute, "CurrentValue").or(start).is_some_and(is_true);
    if value(mute, "Keyframes").is_some_and(|k| !k.trim().is_empty()) {
        warning(report, format!("{context}: track mute automation retained in provenance, not imported; the starting value is used"));
    }
    Ok(muted)
}

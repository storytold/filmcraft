//! Development commands binding verified managed MXF components from retained Avid evidence.
use crate::{EngineError, Result, Services, Session, component_audio, component_media};
use filmcraft_interchange::avid_catalog::{SOURCE_KEY, full_identity, picture_rate_matches};
use filmcraft_media::{AudioStreamInfo, MediaKind};
use filmcraft_project::{AudioChannelMap, AudioChannels, AudioComponent, ItemId, MediaClip, MediaRef, PictureComponent, Project};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};
mod timing;
fn bad(s: impl Into<String>) -> EngineError {
    EngineError::Other(format!("Avid media binding: {}", s.into()))
}
fn ticks(v: &Value, field: &str) -> Result<Tick> {
    let n = v[field].as_i64().filter(|n| *n >= 0).ok_or_else(|| bad("invalid source range"))?;
    let num = v["rate"]["numerator"].as_i64().filter(|n| *n > 0).ok_or_else(|| bad("invalid rate"))?;
    let den = v["rate"]["denominator"].as_i64().filter(|n| *n > 0).ok_or_else(|| bad("invalid rate"))?;
    let scaled = i128::from(n).checked_mul(i128::from(den)).and_then(|n| n.checked_mul(i128::from(TICKS_PER_SECOND))).ok_or_else(|| bad("timing overflow"))?;
    if scaled % i128::from(num) != 0 {
        return Err(bad("source timing cannot be represented exactly"));
    }
    let ticks = i64::try_from(scaled / i128::from(num)).map_err(|_| bad("timing overflow"))?;
    if ticks > Tick::MAX.0 {
        return Err(bad("source range exceeds supported duration"));
    }
    Ok(Tick(ticks))
}
fn direct_source(mut component: &Value) -> Result<&Value> {
    for _ in 0..16 {
        if component.get("unsupported").is_some() || component.get("truncated").is_some_and(|v| v != &Value::Bool(false)) {
            return Err(bad("incomplete source structure"));
        }
        if component["class"] == "SCLP" {
            return Ok(component);
        }
        if component["class"] != "SEQU" {
            break;
        }
        let children = component["components"].as_array().filter(|c| c.len() <= 64).ok_or_else(|| bad("invalid source sequence"))?;
        for child in children {
            if child.get("unsupported").is_some()
                || child.get("truncated").is_some_and(|v| v != &Value::Bool(false))
                || child["rate"] != component["rate"]
                || child["media_kind"] != component["media_kind"]
            {
                return Err(bad("incomplete or incompatible source padding"));
            }
        }
        let mut inputs = children.iter().filter(|c| !(c["class"] == "FILL" && c["length"] == 0));
        let child = inputs.next().ok_or_else(|| bad("empty source sequence"))?;
        if inputs.next().is_some()
            || child["length"] != component["length"]
            || child["rate"] != component["rate"]
            || child["media_kind"] != component["media_kind"]
        {
            return Err(bad("only one direct source with zero-length padding is supported"));
        }
        component = child;
    }
    Err(bad("unsupported source wrapper or nesting limit"))
}
/// Bind at most 64 explicitly chosen master items, one atomic undo step.
pub fn bind(s: &mut Session, p: &Value) -> Result<Value> {
    bind_inner(s, p, false)
}
pub fn bind_media(s: &mut Session, p: &Value) -> Result<Value> {
    bind_inner(s, p, true)
}
fn bind_inner(s: &mut Session, p: &Value, allow_picture: bool) -> Result<Value> {
    let items = p["items"].as_array().filter(|a| !a.is_empty() && a.len() <= 64).ok_or_else(|| bad("provide 1–64 item IDs"))?;
    let ids = items.iter().map(|v| v.as_u64().map(ItemId).ok_or_else(|| bad("invalid item ID"))).collect::<Result<Vec<_>>>()?;
    let plan = prepare_binding(&s.project, &*s.services, &ids, allow_picture, false)?;
    let reports = plan.reports.clone();
    s.edit("Bind Avid Media", move |project, _| plan.apply(project))?;
    for id in ids {
        s.media.remove(id);
    }
    crate::relink::refresh(s);
    Ok(json!({"bound":reports}))
}

/// Validated updates; preparation opens media but never changes project/session state.
pub(crate) struct BindingPlan {
    updates: Vec<(ItemId, MediaClip, String)>,
    pub reports: Vec<Value>,
}
impl BindingPlan {
    /// Apply only to a transaction-local project copy.
    pub fn apply(self, project: &mut Project) -> Result<()> {
        for (id, media, evidence) in self.updates {
            let item = project.item_mut(id).ok_or_else(|| bad("item disappeared"))?;
            *item.as_media_mut().ok_or_else(|| bad("item is not media"))? = media;
            item.metadata.insert(SOURCE_KEY.into(), evidence);
        }
        Ok(())
    }
}
pub(crate) fn prepare_binding(project: &Project, services: &dyn Services, items: &[ItemId], allow_picture: bool, allow_missing: bool) -> Result<BindingPlan> {
    prepare_binding_with_index(project, services, items, allow_picture, allow_missing, None)
}
pub(crate) fn prepare_binding_with_index(
    project: &Project,
    services: &dyn Services,
    items: &[ItemId],
    allow_picture: bool,
    allow_missing: bool,
    media_index: Option<&crate::component_relink::IdentityIndex>,
) -> Result<BindingPlan> {
    if items.is_empty() || items.len() > 64 {
        return Err(bad("provide 1–64 item IDs"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut prepared = Vec::new();
    let mut reports = Vec::new();
    for &id in items {
        if !seen.insert(id) {
            return Err(bad("duplicate item ID"));
        }
        let item = project.item(id).ok_or_else(|| bad("item is missing"))?;
        let mut media = item.as_media().cloned().ok_or_else(|| bad("item is not a media source"))?;
        let raw = item.metadata.get(SOURCE_KEY).filter(|s| s.len() <= 16 * 1024 * 1024).ok_or_else(|| bad("item lacks bounded Avid evidence"))?;
        let mut evidence: Value = serde_json::from_str(raw).map_err(|e| bad(e.to_string()))?;
        let clip = &evidence["clip"];
        let native_descriptors = clip["workflow_evidence"] == "managed_avb_source_descriptors";
        if evidence["version"] != 1 || !(native_descriptors || clip["workflow_evidence"] == "material_identity_in_managed_pmr") {
            return Err(bad("only managed MXF media is supported by this command"));
        }
        let tracks = clip["tracks"].as_array().filter(|a| !a.is_empty() && a.len() <= 64).ok_or_else(|| bad("invalid component tracks"))?;
        let audio_timing = if native_descriptors { timing::reconcile(clip, media_index, services)? } else { None };
        if let Some(timing) = &audio_timing {
            media.info.duration = timing.duration;
        }
        let mut components = Vec::new();
        let mut picture = None;
        let mut reconciliation = None;
        let mut rate = None;
        let mut verified_audio = false;
        let mut companion_rate_needed = false;
        let mut offline_components = 0;
        for t in tracks {
            let index = t["index"].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0).ok_or_else(|| bad("invalid track ID"))?;
            let direct = direct_source(&t["component"])?;
            let is_picture = direct["media_kind"] == 1;
            if !(direct["media_kind"] == 2 || (allow_picture && is_picture)) {
                return Err(bad("only direct audio source clips are supported; picture/wrappers remain unbound"));
            }
            let refs = t["references"].as_array().filter(|a| a.len() == 1).ok_or_else(|| bad("expected one source reference per track"))?;
            let r = &refs[0];
            let unscanned = native_descriptors
                && r["status"] == "unresolved"
                && r["location_scope"] == "no_media_locations_scanned"
                && r["locations"].as_array().is_some_and(Vec::is_empty);
            if !matches!(r["status"].as_str(), Some("present" | "absent")) && !unscanned {
                return Err(bad("ambiguous or unresolved media cannot be bound"));
            }
            let file_package = full_identity(&r["source_id"]).map_err(|e| bad(e.to_string()))?;
            if native_descriptors {
                crate::avid_offline::descriptor(&evidence, file_package)?;
            }
            if full_identity(&direct["source_id"]).map_err(|e| bad(e.to_string()))? != file_package {
                return Err(bad("source identity disagrees with resolved reference"));
            }
            let source_track = direct["source_track"].as_u64().filter(|n| *n > 0 && *n <= u64::from(u32::MAX));
            if source_track.is_none() || source_track != r["source_track"].as_u64() {
                return Err(bad("source track disagrees with resolved reference"));
            }
            if is_picture {
                // Frame counts are reconciled against the independently verified MXF rate below.
                if direct["start"].as_u64().is_none() || direct["start"] != r["source_start"] {
                    return Err(bad("picture source offset disagrees with resolved reference"));
                }
            } else if audio_timing.is_none() && (ticks(direct, "start")? != ticks(r, "source_start")? || ticks(direct, "length")? != ticks(r, "length")?) {
                return Err(bad("audio source range disagrees with resolved reference"));
            }
            let path = if let Some(path) = media_index.and_then(|index| index.unique_path(file_package)) {
                Some(path.to_owned())
            } else if r["status"] == "absent" || unscanned {
                None
            } else {
                let locations = r["locations"].as_array().filter(|a| a.len() == 1).ok_or_else(|| bad("expected a unique file location"))?;
                let location = &locations[0];
                let folder = location["folder"].as_str().filter(|p| p.len() <= 4096 && !p.contains('\0')).ok_or_else(|| bad("invalid media folder"))?;
                let name = location["filename"]
                    .as_str()
                    .filter(|p| !p.is_empty() && p.len() <= 4096 && !p.contains(['/', '\\', '\0', ':']))
                    .ok_or_else(|| bad("invalid component filename"))?;
                let path = std::path::Path::new(folder).join(name);
                if !path.is_absolute() {
                    return Err(bad("component locations must be absolute"));
                }
                Some(path.to_str().ok_or_else(|| bad("non-UTF8 component path"))?.to_owned())
            };
            // An unscanned native source or a genuinely absent file can use its validated
            // descriptor to retain offline structure. Permission,
            // unreadable-file, wrong-package and decoder failures remain errors.
            let missing = match path.as_deref() {
                None => true,
                Some(path) if allow_missing => match services.file_size(path) {
                    Ok(_) => false,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Err(e) => return Err(bad(format!("cannot inspect media: {e}"))),
                },
                Some(_) => false,
            };
            if missing {
                offline_components += 1;
            }
            if is_picture {
                if picture.is_some() {
                    return Err(bad("multiple picture components are unsupported"));
                }
                // Verify package identity/profile before trusting the MXF edit rate. A zero range
                // reads metadata; the final component is separately checked against file duration.
                let pending = missing && allow_missing;
                let video = if pending {
                    let descriptor = crate::avid_offline::descriptor(&evidence, file_package)?;
                    crate::avid_offline::picture(descriptor, r, &evidence, index)?
                } else {
                    let source = component_audio::open_mxf(path.as_deref(), file_package, filmcraft_mxf::TrackKind::Picture, Tick::ZERO, Tick::ZERO, services)?;
                    source.info().video.clone().ok_or_else(|| bad("no decoded picture"))?
                };
                if ![clip, direct, r].iter().all(|v| picture_rate_matches(v, video.frame_rate))
                    || clip["length"] != r["length"]
                    || direct["length"] != r["length"]
                {
                    return Err(bad("picture master and source edit units disagree with verified MXF"));
                }
                let exact = |field: &str| ticks(&json!({(field):r[field],"rate":{"numerator":video.frame_rate.num,"denominator":video.frame_rate.den}}), field);
                let length = exact("length")?;
                let source_start = exact("source_start")?;
                if length != media.info.duration {
                    if tracks.len() != 1 {
                        return Err(bad("rate reconciliation for picture with separate audio is not supported"));
                    }
                    reconciliation = Some(
                        json!({"recorded":clip["rate"],"exact_rate":{"numerator":video.frame_rate.num,"denominator":video.frame_rate.den},"basis":if pending { "automatic_adapter" } else { "identified_mxf" },"length_frames":clip["length"]}),
                    );
                    media.info.duration = length;
                }
                let c = PictureComponent { format_pending: pending, track: index, file_package, path, source_start, length };
                if !pending {
                    component_media::open_picture(&c, services)?;
                }
                media.info.video = Some(video);
                media.info.kind = MediaKind::Movie;
                picture = Some(c);
                continue;
            }
            let component = AudioComponent {
                track: index,
                file_package,
                path,
                stream: 0,
                channel: 0,
                source_start: if audio_timing.is_some() { Tick::ZERO } else { ticks(r, "source_start")? },
                length: if let Some(timing) = &audio_timing {
                    *timing.lengths.get(&index).ok_or_else(|| bad("missing corroborated audio range"))?
                } else {
                    ticks(r, "length")?
                },
            };
            let has_descriptor =
                evidence["source_descriptors"].as_array().is_some_and(|entries| entries.iter().any(|d| full_identity(&d["id"]).ok() == Some(file_package)));
            if missing && allow_missing && r["status"] == "absent" && !has_descriptor {
                // Legacy reports already supported absent channels beside a verified
                // companion. Do not broaden that exception to all-missing sources.
                companion_rate_needed = true;
            } else if missing && allow_missing {
                let descriptor = crate::avid_offline::descriptor(&evidence, file_package)?;
                let sample_rate = crate::avid_offline::audio_rate(descriptor, component.source_start, component.length)?;
                if rate.is_some_and(|r| r != sample_rate) {
                    return Err(bad("mixed sample rates require further validation"));
                }
                rate = Some(sample_rate);
            } else if component.path.is_some() {
                let source = component_audio::open_component(&component, services)?;
                let info = source.info().audio().ok_or_else(|| bad("no decoded audio"))?;
                if info.channels != 1 {
                    return Err(bad("initial Avid binding requires one mono channel per MXF"));
                }
                if rate.is_some_and(|r| r != info.sample_rate) {
                    return Err(bad("mixed sample rates require further validation"));
                }
                rate = Some(info.sample_rate);
                verified_audio = true;
            }
            components.push(component);
        }
        components.sort_by_key(|c| c.track);
        if components.iter().enumerate().any(|(i, c)| c.track as usize != i + 1) {
            return Err(bad("logical audio tracks must include every slot from A1"));
        }
        let sample_rate = rate.unwrap_or(48000);
        if (rate.is_none() && !components.is_empty()) || (companion_rate_needed && !verified_audio) {
            return Err(bad("no playable audio component found"));
        }
        let grouped = match clip["attributes"]["_CHANNEL_GROUP_LIST"].as_str() {
            None | Some("") => false,
            Some("ST:A1A2,") if components.len() == 2 => true,
            Some(_) => return Err(bad("this channel grouping is not supported yet")),
        };
        if !components.is_empty() {
            media.info.audio_streams =
                vec![AudioStreamInfo { sample_rate, channels: components.len() as u32, codec: "MXF component PCM".into(), bits_per_sample: None }];
            media.interpret.audio_channels =
                Some(AudioChannelMap::for_format(if grouped { AudioChannels::Stereo } else { AudioChannels::Mono }, components.len() as u16));
        } else {
            media.info.audio_streams.clear();
            media.interpret.audio_channels = None;
        }
        let (_, mut failures) = component_media::open(media.info.clone(), &components, picture.as_ref(), services)?;
        if let Some(note) = reconciliation {
            evidence["picture_rate_reconciliation"] = note;
            failures
                .push("Picture frame counts use the corroborated exact rate; original Avid rate and interpretation basis remain in source evidence.".into());
        }
        let audio_reconciled = audio_timing.is_some();
        if let Some(timing) = audio_timing {
            evidence["audio_rate_reconciliation"] = timing.evidence;
            failures.push("Audio timing uses Avid's nominal 23.976 clock corroborated by identified MXF track sample counts; raw rates and counts are preserved. Component ends are limited to declared whole samples, not stored padding.".into());
        }
        reports.push(json!({"item":id,"channels":components.len(),"picture":picture.is_some(),"offlineComponents":offline_components,"warnings":failures,"audioTimingReconciled":audio_reconciled}));
        media.media = MediaRef::Components { channels: components, picture };
        media.info.container = "MXF components".into();
        media.offline = false;
        media.proxy = None;
        media.identity = None;
        evidence["binding"] = json!("media_components");
        prepared.push((id, media, evidence.to_string()));
    }
    Ok(BindingPlan { updates: prepared, reports })
}

pub fn relink_component(s: &mut Session, p: &Value) -> Result<Value> {
    let id = ItemId(p["item"].as_u64().ok_or_else(|| bad("need item ID"))?);
    let channel = p["channel"].as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("need zero-based channel index"))?;
    let path = p["path"].as_str().filter(|p| std::path::Path::new(p).is_absolute()).ok_or_else(|| bad("need absolute file path"))?;
    crate::component_relink::relink(s, &json!({"bindings":[{"item":id,"component":{"kind":"audio","channel":channel},"path":path}]}))?;
    Ok(json!({"item":id,"channel":channel}))
}

pub fn relink_picture(s: &mut Session, p: &Value) -> Result<Value> {
    let id = ItemId(p["item"].as_u64().ok_or_else(|| bad("need item ID"))?);
    let path = p["path"].as_str().ok_or_else(|| bad("need file path"))?;
    crate::component_relink::relink(s, &json!({"bindings":[{"item":id,"component":{"kind":"picture"},"path":path}]}))?;
    Ok(json!({"item":id}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avid_audit_source_rejects_incomplete_or_incompatible_padding() {
        let rate = json!({"numerator":24,"denominator":1});
        for (key, value) in
            [("truncated", json!(true)), ("unsupported", json!("unknown")), ("media_kind", json!(2)), ("rate", json!({"numerator":25,"denominator":1}))]
        {
            let mut padding = json!({"class":"FILL","length":0,"media_kind":1,"rate":rate});
            padding[key] = value;
            let wrapped =
                json!({"class":"SEQU","length":24,"media_kind":1,"rate":rate,"components":[padding,{"class":"SCLP","length":24,"media_kind":1,"rate":rate}]});
            assert!(direct_source(&wrapped).is_err(), "accepted padding {key}");
        }
    }
    #[test]
    fn direct_source_accepts_only_complete_single_source_padding() {
        let leaf = json!({"class":"SCLP","media_kind":1,"length":24,"rate":{"numerator":24,"denominator":1}});
        let wrapped = json!({"class":"SEQU","media_kind":1,"length":24,"rate":leaf["rate"],"components":[{"class":"FILL","length":0,"media_kind":1,"rate":leaf["rate"]},leaf]});
        assert_eq!(direct_source(&wrapped).unwrap()["class"], "SCLP");
        let mut bad = wrapped.clone();
        bad["components"][0]["length"] = json!(1);
        assert!(direct_source(&bad).is_err());
        let mut bad = wrapped.clone();
        bad["truncated"] = json!(true);
        assert!(direct_source(&bad).is_err());
        let mut deep = wrapped;
        for _ in 0..20 {
            deep = json!({"class":"SEQU","length":24,"media_kind":1,"rate":deep["rate"],"components":[deep]});
        }
        assert!(direct_source(&deep).is_err());
    }
}

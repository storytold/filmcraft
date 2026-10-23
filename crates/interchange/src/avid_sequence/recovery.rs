//! Explicit best-effort recovery: preserve a known interval; never invent source identities.
use super::*;
impl Walk<'_> {
    pub(super) fn node(&mut self, c: &Value, start: Tick, depth: usize) -> Result<Vec<ClipPlan>> {
        match self.checked_node(c, start, depth) {
            Ok(v) => Ok(v),
            Err(error) if self.best_effort && *self.nodes <= MAX_NODES && depth < 16 => {
                let length = self.clock.ticks(c, "length")?;
                if length <= Tick::ZERO || self.clock.rate(c)? != self.rate {
                    return Err(error);
                }
                add(start, length)?;
                let class = c["class"].as_str().unwrap_or("unknown");
                // A group selector chooses media; it is never a transparent effect. A
                // two-input TKFX is safe to bypass only when its first input is plain
                // filler and the second is the sole foreground source. Effects with a
                // real background must remain placeholders instead of losing a layer.
                let foreground = match c["input_tracks"].as_array() {
                    Some(inputs) if class != "SLCT" && inputs.len() == 1 && inputs[0]["index"] == 1 => Some((&inputs[0]["component"], false)),
                    Some(inputs)
                        if class == "TKFX"
                            && inputs.len() == 2
                            && inputs[0]["index"] == 1
                            && inputs[1]["index"] == 2
                            && c["properties"]["mc_mode"] == 0
                            && self.plain_filler(&inputs[0]["component"], length, depth + 1).unwrap_or(false) =>
                    {
                        Some((&inputs[1]["component"], true))
                    }
                    _ => None,
                };
                if let Some((child, require_one_clip)) = foreground
                    && self.clock.ticks(child, "length").ok() == Some(length)
                    && self.clock.rate(child).ok() == Some(self.rate)
                    && child["media_kind"] == c["media_kind"]
                {
                    let clips = self.node(child, start, depth + 1)?;
                    let end = add(start, length)?;
                    let valid = if require_one_clip {
                        clips.len() == 1 && clips[0].placeholder.is_none() && clips[0].start == start && clips[0].duration == length
                    } else {
                        !clips.is_empty()
                            && clips.iter().all(|clip| {
                                clip.placeholder.is_none() && clip.start >= start && add(clip.start, clip.duration).ok().is_some_and(|clip_end| clip_end <= end)
                            })
                    };
                    if valid {
                        self.warnings
                            .push(format!("Frame {}: {class} effect skipped; underlying clip imported without it. {error}", self.rate.frame_at(start)));
                        return Ok(clips);
                    }
                }
                let message = format!("Frame {}: offline placeholder for {class}; {error}", self.rate.frame_at(start));
                self.warnings.push(message.clone());
                Ok(vec![ClipPlan {
                    item: ItemId(0),
                    placeholder: Some(message),
                    start,
                    source_in: Tick::ZERO,
                    duration: length,
                    channels: if self.kind == TrackKind::Audio { (0..self.audio_channels as u16).collect() } else { Vec::new() },
                    scale_to_frame: false,
                    effects: Vec::new(),
                    sampled_source_offsets: Vec::new(),
                }])
            }
            Err(e) => Err(e),
        }
    }

    fn plain_filler(&self, c: &Value, length: Tick, depth: usize) -> Result<bool> {
        if depth >= 16
            || self.clock.ticks(c, "length")? != length
            || self.clock.rate(c)? != self.rate
            || c["media_kind"] != if self.kind == TrackKind::Video { 1 } else { 2 }
        {
            return Ok(false);
        }
        match c["class"].as_str() {
            Some("FILL") => Ok(true),
            Some("SEQU") => {
                let mut total = Tick::ZERO;
                for child in list(c, "components", MAX_NODES)? {
                    let span = self.clock.ticks(child, "length")?;
                    if child["class"] != "FILL" || self.clock.rate(child)? != self.rate || child["media_kind"] != c["media_kind"] {
                        return Ok(false);
                    }
                    total = add(total, span)?;
                }
                Ok(total == length)
            }
            _ => Ok(false),
        }
    }
}
pub(super) fn placeholder(p: &mut Project, bin: BinId, kind: TrackKind, stereo: bool, duration: Tick, reason: &str) -> Result<ItemId> {
    use filmcraft_media::{AudioStreamInfo, MediaInfo, MediaKind};
    use filmcraft_project::{Interpretation, ItemKind, Label, MediaClip};
    let name = "Unconverted Avid segment";
    let id = p.add_item(
        name,
        Label::Rose,
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: String::new() },
            info: MediaInfo {
                name: name.into(),
                kind: if kind == TrackKind::Video { MediaKind::Movie } else { MediaKind::AudioOnly },
                duration,
                video: None,
                audio_streams: if kind == TrackKind::Audio {
                    vec![AudioStreamInfo {
                        sample_rate: 48000,
                        channels: if stereo { 2 } else { 1 },
                        codec: "Offline placeholder".into(),
                        bits_per_sample: None,
                    }]
                } else {
                    Vec::new()
                },
                container: "Unconverted Avid segment".into(),
                start_timecode: None,
                file_size: None,
            },
            interpret: Interpretation::default(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            offline: true,
            proxy: None,
            identity: None,
        }),
        Some(bin),
    );
    p.item_mut(id).ok_or_else(|| bad("placeholder disappeared"))?.metadata.insert("filmcraft:avid-import-error".into(), reason.into());
    Ok(id)
}

/// Last resort when individual tracks cannot be interpreted but the composition clock and
/// duration are valid. Keep a visible sequence for inspection without inventing any edits.
pub(super) fn sequence_placeholder(m: &Value, error: &Error) -> Result<SequencePlan> {
    let format = formats::resolve(m, true)?;
    let duration = format.clock.ticks(m, "length")?;
    if duration <= Tick::ZERO {
        return Err(bad("cannot preserve an empty or invalid composition range"));
    }
    let settings = format.settings;
    settings.validate().map_err(bad)?;
    let reason = format!("Entire sequence represented by an offline placeholder: {error}");
    Ok(SequencePlan {
        name: text(m, "name")?.into(),
        settings,
        start_timecode: 0,
        duration,
        tracks: vec![TrackPlan {
            kind: TrackKind::Video,
            number: 1,
            stereo: false,
            muted: false,
            solo: false,
            clips: vec![ClipPlan {
                item: ItemId(0),
                placeholder: Some(reason.clone()),
                start: Tick::ZERO,
                source_in: Tick::ZERO,
                duration,
                channels: Vec::new(),
                scale_to_frame: false,
                effects: Vec::new(),
                sampled_source_offsets: Vec::new(),
            }],
        }],
        evidence: m.clone(),
        warnings: format
            .warnings
            .into_iter()
            .chain([reason, "Sequence start timecode uses zero; original evidence is retained for debugging.".into()])
            .collect(),
    })
}

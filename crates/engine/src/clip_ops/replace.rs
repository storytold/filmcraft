use super::common::item_rate;
use super::enabled::is_clip_item;
use super::file::item_range;
use super::*;

// ---------------------------------------------------------------------------------------------
// Clip: Replace With Clip
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Replace {
    Source,
    MatchFrame,
    Bin,
}

pub(super) fn replace_with(s: &mut Session, p: &Value, how: Replace) -> Result<Value> {
    let cmd = match how {
        Replace::Source => "clip.replaceFromSource",
        Replace::MatchFrame => "clip.replaceFromSourceMatchFrame",
        Replace::Bin => "clip.replaceFromBin",
    };
    let new_item = match how {
        Replace::Bin => item_p(p, "item").or_else(|| s.state.project_selection.iter().copied().find(|i| is_clip_item(&s.project, *i))),
        _ => s.state.source_item,
    }
    .ok_or_else(|| bad(cmd, "no replacement clip"))?;
    let pi = s.project.item(new_item).ok_or_else(|| bad(cmd, "no such item"))?.clone();
    let (has_v, has_a) = (pi.has_video(), pi.has_audio());
    // where the replacement starts in its media: Source In (or the subclip / item start)
    let in_point = match how {
        Replace::Bin => item_range(&s.project, new_item, s.prefs.timeline.still_duration(s.sequence_rate())).map(|r| r.start).unwrap_or_default(),
        _ => source_range(s).map(|(_, r)| r.start).unwrap_or_default(),
    };
    // From Bin starts at the item's In point, as Premiere does; `keepSourceIn` keeps each clip's own
    let keep_in = how == Replace::Bin && bool_p(p, "keepSourceIn").unwrap_or(false);
    // ...but a subclip that restricts trims has no media outside its range: a kept In stays on
    // one of its frames (first frame, last frame)
    let keep_within = match &pi.kind {
        ItemKind::Subclip { range, restrict_trims: true, .. } => {
            Some((range.start, (range.end() - item_rate(&s.project, new_item).frame_duration()).max(range.start)))
        }
        _ => None,
    };
    let src_ph = s.state.source_playhead;
    let ph = s.playhead();
    let targets = with_links(s, &clips_p(s, p));
    let src_channels = pi.as_media().and_then(|m| m.interpret.audio_channels.as_ref()).and_then(|a| a.clips.first().cloned()).unwrap_or_default();
    let (replaced, clamped) = s.edit_sequence("Replace With Clip", |q, _, _| {
        let (mut out, mut clamped) = (Vec::new(), Vec::new());
        for c in &targets {
            let Some((tid, _)) = q.find_item(*c) else { continue };
            let kind = q.track(tid).map(|t| t.kind).unwrap_or_default();
            if (kind == TrackKind::Video && !has_v) || (kind == TrackKind::Audio && !has_a) {
                continue;
            }
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let src_in = match how {
                // the Source monitor frame lines up with the sequence playhead (reverse-aware)
                Replace::MatchFrame => src_ph - (it.moving_source_time_at(ph) - it.source_in),
                _ if keep_in => {
                    let kept = keep_within.map(|(first, last)| it.source_in.clamp(first, last)).unwrap_or(it.source_in);
                    if kept != it.source_in {
                        clamped.push(c.0);
                    }
                    kept
                }
                _ => in_point,
            };
            if src_in < Tick::ZERO {
                return Err(EngineError::Other("not enough media before the Source monitor playhead to match the frame".into()));
            }
            it.item = new_item;
            it.name = pi.name.clone();
            it.label = pi.label;
            it.source_in = src_in;
            it.frame_hold = None;
            it.multicam = None;
            if kind == TrackKind::Audio {
                it.source_channels = src_channels.clone();
            }
            out.push(c.0);
        }
        if out.is_empty() {
            return Err(EngineError::Other("the replacement has no video or audio for the selected clips".into()));
        }
        Ok((out, clamped))
    })?;
    let mut result = json!({"clips": replaced, "item": new_item.0, "short": short_clips(s, &replaced)});
    if keep_in {
        // the clips whose In was outside a trims-restricted subclip and now sits on its nearest frame
        result["sourceInClamped"] = json!(clamped);
    }
    Ok(result)
}

/// The clips among `clips` that play past the end of their media, with how many sequence frames
/// of the clip have no media (at most the clip's own length). The edit is left as it is (the
/// renderer holds the last frame there); this is so whoever replaced the clip knows.
fn short_clips(s: &Session, clips: &[u64]) -> Vec<Value> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let frame = q.settings.frame_rate.frame_duration().0.max(1);
    clips
        .iter()
        .filter_map(|c| {
            let (_, it) = q.find_item(ClipId(*c))?;
            // stills, mattes and other sources without an end are never short; nor is a held frame
            let media_end = crate::media_duration(&s.project, &s.media, it.item)?;
            let over = it.source_out().0.saturating_sub(media_end.0);
            if over <= 0 || it.frame_hold.is_some() {
                return None;
            }
            // media ticks past the end, as sequence ticks of this clip
            let length = it.duration.0.max(0);
            let ticks = ((over as f64 / it.speed.abs().max(1e-9)).round().min(length as f64) as i64).min(length);
            let frames = ticks.saturating_add(frame - 1) / frame;
            (frames > 0).then(|| json!({"clip": c, "shortByFrames": frames}))
        })
        .collect()
}

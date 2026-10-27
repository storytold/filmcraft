use super::common::{collapse_history, items_p, media_root};
use super::enabled::extract_items;
use super::*;

// ---------------------------------------------------------------------------------------------
// Clip: Audio Options
// ---------------------------------------------------------------------------------------------

pub(super) fn breakout_to_mono(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> =
        items_p(s, p).into_iter().filter(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some_and(|m| m.info.has_audio())).collect();
    if items.is_empty() {
        return Err(bad("clip.breakoutToMono", "select a clip with audio in the Project panel"));
    }
    let made = s.edit("Breakout to Mono", |pr, st| {
        let mut made = Vec::new();
        for i in &items {
            let Some(it) = pr.item(*i).cloned() else { continue };
            let Some(m) = it.as_media() else { continue };
            let n = m.info.audio().map_or(1, |a| a.channels).max(1) as u16;
            let bin = pr.root.parent_of(*i).filter(|b| *b != pr.root.id);
            for c in 0..n {
                let suffix = match (n, c) {
                    (2, 0) => "Left".to_string(),
                    (2, 1) => "Right".to_string(),
                    _ => format!("Ch {}", c + 1),
                };
                let mut mm = m.clone();
                mm.interpret.audio_channels = Some(AudioChannelMap { format: AudioChannels::Mono, clips: vec![vec![c]] });
                mm.mark_in = None;
                mm.mark_out = None;
                mm.markers.clear();
                mm.proxy = None;
                let id = pr.add_item(&format!("{} {suffix}", it.name), it.label, ItemKind::Media(mm), bin);
                made.push(id);
            }
        }
        st.project_selection = made.clone();
        Ok(made)
    })?;
    Ok(json!({"items": made.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

pub(super) fn extract_audio(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match p.get("items") {
        Some(_) => items_p(s, p),
        None => extract_items(s),
    };
    if items.is_empty() {
        return Err(bad("clip.extractAudio", "select a clip with audio"));
    }
    let n0 = s.history.undo.len();
    let mut out = Vec::new();
    for item in items {
        let (root, m, sub) = media_root(&s.project, item).ok_or_else(|| bad("clip.extractAudio", "not a media clip"))?;
        let a = m.info.audio().cloned().ok_or_else(|| bad("clip.extractAudio", "the clip has no audio"))?;
        let range = sub.unwrap_or(TimeRange::new(Tick::ZERO, m.info.duration));
        let dir = match str_p(p, "dir") {
            Some(d) => d.to_string(),
            None => s
                .path
                .as_deref()
                .and_then(|x| std::path::Path::new(x).parent())
                .map(|d| d.to_string_lossy().into_owned())
                .or_else(|| match &m.media {
                    MediaRef::File { path } => std::path::Path::new(path).parent().map(|d| d.to_string_lossy().into_owned()),
                    _ => None,
                })
                .ok_or_else(|| EngineError::Other("save the project first: extracted audio is written next to it".into()))?,
        };
        let pi = s.project.item(item).ok_or_else(|| EngineError::Other("no such item".into()))?;
        let name = pi.name.clone();
        let stem = std::path::Path::new(&name).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
        let src = s.media.full_res_source(&s.project, root, &*s.services).ok_or_else(|| EngineError::Other(format!("{name}: media is offline")))?;
        let sr = a.sample_rate.max(8000);
        let start = range.start.to_units_floor(sr as i64);
        let frames = (range.end().to_units_floor(sr as i64) - start).max(0) as usize;
        let buf = src.audio(start, frames, sr).map_err(|e| EngineError::Other(format!("{name}: {e}")))?;
        let ch = buf.channel_count().max(1);
        let mut inter = Vec::with_capacity(frames * ch);
        for i in 0..frames {
            for c in 0..ch {
                inter.push(buf.channels.get(c).and_then(|x| x.get(i)).copied().unwrap_or(0.0));
            }
        }
        let bytes = filmcraft_media::wav::write_wav16(&inter, ch as u16, sr);
        let mut path = std::path::Path::new(&dir).join(format!("{stem} Audio Extracted.wav"));
        let mut k = 2;
        while s.services.file_size(&path.to_string_lossy()).is_ok() {
            path = std::path::Path::new(&dir).join(format!("{stem} Audio Extracted {k}.wav"));
            k += 1;
        }
        let path_s = path.to_string_lossy().into_owned();
        s.services.write_file(&path_s, &bytes).map_err(|e| EngineError::Other(format!("{path_s}: {e}")))?;
        let bin = s.project.root.parent_of(item).filter(|b| *b != s.project.root.id);
        let id = crate::commands::import_bytes(s, &path_s, bytes.into(), bin)?;
        out.push(json!({"item": id.0, "path": path_s, "from": item.0}));
    }
    collapse_history(s, n0, "Extract Audio");
    Ok(json!({"extracted": out}))
}

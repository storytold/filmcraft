//! Conversion between FilmCraft effect values and the shared timeline model.

use craft_timeline::model as shared;
use filmcraft_project as film;
use filmcraft_time::Tick;

fn to_value(value: &film::ParamValue) -> Result<shared::ParamValue, serde_json::Error> {
    Ok(match value {
        film::ParamValue::Float(v) => shared::ParamValue::Float(*v),
        film::ParamValue::Vec2(v) => shared::ParamValue::Vec2(shared::Vec2::new(v.x, v.y)),
        film::ParamValue::Color(v) => shared::ParamValue::Color(*v),
        film::ParamValue::Bool(v) => shared::ParamValue::Bool(*v),
        film::ParamValue::Choice(v) => shared::ParamValue::Choice(*v),
        film::ParamValue::Text(v) => shared::ParamValue::Text(v.clone()),
        film::ParamValue::Curve(_) | film::ParamValue::Path(_) => shared::ParamValue::Opaque(serde_json::to_string(value)?),
    })
}

fn from_value(value: &shared::ParamValue) -> Result<film::ParamValue, serde_json::Error> {
    Ok(match value {
        shared::ParamValue::Float(v) => film::ParamValue::Float(*v),
        shared::ParamValue::Vec2(v) => film::ParamValue::Vec2(filmcraft_geom::Vec2::new(v.x, v.y)),
        shared::ParamValue::Color(v) => film::ParamValue::Color(*v),
        shared::ParamValue::Bool(v) => film::ParamValue::Bool(*v),
        shared::ParamValue::Choice(v) => film::ParamValue::Choice(*v),
        shared::ParamValue::Text(v) => film::ParamValue::Text(v.clone()),
        shared::ParamValue::Opaque(v) => serde_json::from_str(v)?,
    })
}

fn to_interp(value: film::Interpolation) -> shared::Interpolation {
    match value {
        film::Interpolation::Linear => shared::Interpolation::Linear,
        film::Interpolation::Hold => shared::Interpolation::Hold,
        film::Interpolation::Bezier => shared::Interpolation::Bezier,
        film::Interpolation::AutoBezier => shared::Interpolation::AutoBezier,
        film::Interpolation::ContinuousBezier => shared::Interpolation::ContinuousBezier,
        film::Interpolation::EaseIn => shared::Interpolation::EaseIn,
        film::Interpolation::EaseOut => shared::Interpolation::EaseOut,
    }
}

fn from_interp(value: shared::Interpolation) -> film::Interpolation {
    match value {
        shared::Interpolation::Linear => film::Interpolation::Linear,
        shared::Interpolation::Hold => film::Interpolation::Hold,
        shared::Interpolation::Bezier => film::Interpolation::Bezier,
        shared::Interpolation::AutoBezier => film::Interpolation::AutoBezier,
        shared::Interpolation::ContinuousBezier => film::Interpolation::ContinuousBezier,
        shared::Interpolation::EaseIn => film::Interpolation::EaseIn,
        shared::Interpolation::EaseOut => film::Interpolation::EaseOut,
    }
}

fn to_effect(effect: &film::EffectInstance) -> Result<shared::EffectInstance, serde_json::Error> {
    let params = effect
        .params
        .iter()
        .map(|(name, param)| {
            let keyframes = param
                .keyframes
                .iter()
                .map(|k| {
                    Ok(shared::Keyframe {
                        time: craft_timeline::time::Tick(k.time.0),
                        value: to_value(&k.value)?,
                        interp: to_interp(k.interp),
                        out_influence: k.out_influence,
                        in_influence: k.in_influence,
                    })
                })
                .collect::<Result<Vec<_>, serde_json::Error>>()?;
            Ok((name.clone(), shared::Param { value: to_value(&param.value)?, keyframes }))
        })
        .collect::<Result<_, serde_json::Error>>()?;
    // Shared OTIO emits extension fields after the shared fields; retain FilmCraft's effect-only data.
    let ext = serde_json::json!({"masks": effect.masks, "post_fader": effect.post_fader, "essential": effect.essential, "layer": effect.layer});
    Ok(shared::EffectInstance { effect: effect.effect.clone(), enabled: effect.enabled, params, ext: Some(serde_json::to_string(&ext)?) })
}

fn from_effect(effect: &shared::EffectInstance) -> Result<film::EffectInstance, serde_json::Error> {
    let params: std::collections::BTreeMap<String, film::Param> = effect
        .params
        .iter()
        .map(|(name, param)| {
            let keyframes = param
                .keyframes
                .iter()
                .map(|k| {
                    Ok(film::Keyframe {
                        time: Tick(k.time.0),
                        value: from_value(&k.value)?,
                        interp: from_interp(k.interp),
                        out_influence: k.out_influence,
                        in_influence: k.in_influence,
                    })
                })
                .collect::<Result<Vec<_>, serde_json::Error>>()?;
            Ok((name.clone(), film::Param { value: from_value(&param.value)?, keyframes }))
        })
        .collect::<Result<_, serde_json::Error>>()?;
    let ext: serde_json::Value = effect.ext.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| serde_json::json!({}));
    Ok(film::EffectInstance {
        effect: effect.effect.clone(),
        enabled: effect.enabled,
        params,
        masks: ext.get("masks").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
        post_fader: ext.get("post_fader").and_then(serde_json::Value::as_bool).unwrap_or(false),
        essential: ext.get("essential").and_then(serde_json::Value::as_bool).unwrap_or(false),
        layer: ext.get("layer").and_then(|v| serde_json::from_value(v.clone()).ok()),
    })
}

fn to_marker(marker: &film::Marker) -> Result<shared::Marker, serde_json::Error> {
    Ok(shared::Marker {
        id: shared::MarkerId(marker.id.0),
        start: craft_timeline::time::Tick(marker.start.0),
        duration: craft_timeline::time::Tick(marker.duration.0),
        name: marker.name.clone(),
        comment: marker.comment.clone(),
        kind: match marker.kind {
            film::MarkerKind::Comment => shared::MarkerKind::Comment,
            film::MarkerKind::Chapter => shared::MarkerKind::Chapter,
            film::MarkerKind::Segmentation => shared::MarkerKind::Segmentation,
            film::MarkerKind::WebLink => shared::MarkerKind::WebLink,
            film::MarkerKind::FlashCue => shared::MarkerKind::FlashCue,
        },
        color: shared::Label::from_name(marker.color.name()).unwrap_or(shared::Label::Green),
        url: marker.url.clone(),
        author: marker.author.clone(),
        ext: Some(serde_json::to_string(&serde_json::json!({"custom_color": marker.custom_color, "filmcraft_marker_id": marker.id.0}))?),
    })
}

fn from_marker(marker: &shared::Marker) -> Result<film::Marker, serde_json::Error> {
    let ext: serde_json::Value = match &marker.ext {
        Some(v) => serde_json::from_str(v)?,
        None => serde_json::json!({}),
    };
    Ok(film::Marker {
        id: film::MarkerId(marker.id.0),
        start: Tick(marker.start.0),
        duration: Tick(marker.duration.0),
        name: marker.name.clone(),
        comment: marker.comment.clone(),
        kind: match marker.kind {
            shared::MarkerKind::Comment => film::MarkerKind::Comment,
            shared::MarkerKind::Chapter => film::MarkerKind::Chapter,
            shared::MarkerKind::Segmentation => film::MarkerKind::Segmentation,
            shared::MarkerKind::WebLink => film::MarkerKind::WebLink,
            shared::MarkerKind::FlashCue => film::MarkerKind::FlashCue,
        },
        color: film::Label::from_name(marker.color.name()).unwrap_or(film::Label::Green),
        url: marker.url.clone(),
        author: marker.author.clone(),
        custom_color: serde_json::from_value(ext.get("custom_color").cloned().unwrap_or(serde_json::Value::Null))?,
    })
}

// The neutral interchange model carries only the settings common to timeline formats.
// The complete FilmCraft value is retained separately in item metadata by the adapter.
fn to_settings(settings: &film::SequenceSettings) -> shared::SequenceSettings {
    shared::SequenceSettings {
        width: settings.width,
        height: settings.height,
        frame_rate: settings.frame_rate,
        par: settings.par,
        sample_rate: settings.sample_rate,
        drop_frame: settings.drop_frame,
    }
}

fn from_settings(settings: &shared::SequenceSettings) -> film::SequenceSettings {
    film::SequenceSettings {
        width: settings.width,
        height: settings.height,
        frame_rate: settings.frame_rate,
        par: settings.par,
        sample_rate: settings.sample_rate,
        drop_frame: settings.drop_frame,
        ..film::SequenceSettings::default()
    }
}

fn to_channels(channels: film::AudioChannels) -> shared::AudioChannels {
    match channels {
        film::AudioChannels::Mono => shared::AudioChannels::Mono,
        film::AudioChannels::Stereo => shared::AudioChannels::Stereo,
        film::AudioChannels::Surround51 => shared::AudioChannels::Surround51,
        film::AudioChannels::Adaptive => shared::AudioChannels::Adaptive,
    }
}

fn from_channels(channels: shared::AudioChannels) -> film::AudioChannels {
    match channels {
        shared::AudioChannels::Mono => film::AudioChannels::Mono,
        shared::AudioChannels::Stereo => film::AudioChannels::Stereo,
        shared::AudioChannels::Surround51 => film::AudioChannels::Surround51,
        shared::AudioChannels::Adaptive => film::AudioChannels::Adaptive,
    }
}

fn to_track_kind(kind: film::TrackKind) -> shared::TrackKind {
    match kind {
        film::TrackKind::Video => shared::TrackKind::Video,
        film::TrackKind::Audio => shared::TrackKind::Audio,
    }
}
fn from_track_kind(kind: shared::TrackKind) -> film::TrackKind {
    match kind {
        shared::TrackKind::Video => film::TrackKind::Video,
        shared::TrackKind::Audio => film::TrackKind::Audio,
    }
}
fn to_align(align: film::TransitionAlign) -> shared::TransitionAlign {
    match align {
        film::TransitionAlign::CenterAtCut => shared::TransitionAlign::CenterAtCut,
        film::TransitionAlign::StartAtCut => shared::TransitionAlign::StartAtCut,
        film::TransitionAlign::EndAtCut => shared::TransitionAlign::EndAtCut,
    }
}
fn from_align(align: shared::TransitionAlign) -> film::TransitionAlign {
    match align {
        shared::TransitionAlign::CenterAtCut => film::TransitionAlign::CenterAtCut,
        shared::TransitionAlign::StartAtCut => film::TransitionAlign::StartAtCut,
        shared::TransitionAlign::EndAtCut => film::TransitionAlign::EndAtCut,
    }
}
fn to_track_item(item: &film::TrackItem) -> Result<shared::TrackItem, serde_json::Error> {
    Ok(shared::TrackItem {
        id: shared::ClipId(item.id.0),
        item: shared::ItemId(item.item.0),
        name: item.name.clone(),
        label: shared::Label::from_name(item.label.name()).unwrap_or(shared::Label::Iris),
        start: item.start,
        duration: item.duration,
        source_in: item.source_in,
        speed: item.speed,
        reverse: item.reverse,
        enabled: item.enabled,
        link: item.link,
        group: item.group,
        effects: item.effects.iter().map(to_effect).collect::<Result<_, _>>()?,
        markers: item.markers.iter().map(to_marker).collect::<Result<_, _>>()?,
        gain_db: item.gain_db,
        frame_hold: item.frame_hold,
        scale_to_frame: item.scale_to_frame,
        source_channels: item.source_channels.clone(),
        ext: Some(serde_json::json!({"custom_label_color":item.custom_label_color}).to_string()),
    })
}
fn to_transition(t: &film::Transition) -> Result<shared::Transition, serde_json::Error> {
    Ok(shared::Transition {
        id: shared::TransitionId(t.id.0),
        effect: to_effect(&t.effect)?,
        start: t.start,
        duration: t.duration,
        from: t.from.map(|id| shared::ClipId(id.0)),
        to: t.to.map(|id| shared::ClipId(id.0)),
        align: to_align(t.align),
        reverse: t.reverse,
        ext: None,
    })
}
fn to_track(t: &film::Track) -> Result<shared::Track, serde_json::Error> {
    Ok(shared::Track {
        id: shared::TrackId(t.id.0),
        kind: to_track_kind(t.kind),
        name: t.name.clone(),
        items: t.items.iter().map(to_track_item).collect::<Result<_, _>>()?,
        transitions: t.transitions.iter().map(to_transition).collect::<Result<_, _>>()?,
        locked: t.locked,
        enabled: t.enabled,
        channels: to_channels(t.channels),
        ext: Some(
            serde_json::json!({"sync_lock":t.sync_lock,"muted":t.muted,"solo":t.solo,"volume_db":t.volume_db,"pan":t.pan,"effects":t.effects}).to_string(),
        ),
    })
}
fn to_sequence(s: &film::Sequence) -> Result<shared::Sequence, serde_json::Error> {
    Ok(shared::Sequence {
        settings: to_settings(&s.settings),
        video_tracks: s.video_tracks.iter().map(to_track).collect::<Result<_, _>>()?,
        audio_tracks: s.audio_tracks.iter().map(to_track).collect::<Result<_, _>>()?,
        markers: s.markers.iter().map(to_marker).collect::<Result<_, _>>()?,
        start_timecode: s.start_timecode,
    })
}

fn from_track_item(item: &shared::TrackItem, kind: film::TrackKind, builder: &mut crate::common::Builder) -> Result<film::TrackItem, serde_json::Error> {
    let mut out = builder.clip(film::ItemId(item.item.0), kind, &item.name, item.start, item.duration, item.source_in);
    if let Some(meta) = legacy_or_ext(&item.ext) {
        out.custom_label_color = meta.get("custom_label_color").and_then(|v| serde_json::from_value(v.clone()).ok());
    }
    out.id = film::ClipId(item.id.0);
    out.item = film::ItemId(item.item.0);
    out.name = item.name.clone();
    out.label = film::Label::from_name(item.label.name()).unwrap_or(film::Label::Iris);
    out.start = item.start;
    out.duration = item.duration;
    out.source_in = item.source_in;
    out.speed = item.speed;
    out.reverse = item.reverse;
    out.enabled = item.enabled;
    out.link = item.link;
    out.group = item.group;
    out.effects = item.effects.iter().map(from_effect).collect::<Result<_, _>>()?;
    out.markers = item.markers.iter().map(from_marker).collect::<Result<_, _>>()?;
    out.gain_db = item.gain_db;
    out.frame_hold = item.frame_hold;
    out.scale_to_frame = item.scale_to_frame;
    out.source_channels = item.source_channels.clone();
    Ok(out)
}
fn from_transition(t: &shared::Transition) -> Result<film::Transition, serde_json::Error> {
    Ok(film::Transition {
        id: film::TransitionId(t.id.0),
        effect: from_effect(&t.effect)?,
        start: t.start,
        duration: t.duration,
        from: t.from.map(|id| film::ClipId(id.0)),
        to: t.to.map(|id| film::ClipId(id.0)),
        align: from_align(t.align),
        reverse: t.reverse,
    })
}
fn from_track(t: &shared::Track, builder: &mut crate::common::Builder) -> Result<film::Track, serde_json::Error> {
    let kind = from_track_kind(t.kind);
    let mut out = film::Track::new(film::TrackId(t.id.0), kind, t.name.clone());
    if let Some(meta) = legacy_or_ext(&t.ext) {
        out.sync_lock = meta.get("sync_lock").and_then(serde_json::Value::as_bool).unwrap_or(out.sync_lock);
        out.muted = meta.get("muted").and_then(serde_json::Value::as_bool).unwrap_or(out.muted);
        out.solo = meta.get("solo").and_then(serde_json::Value::as_bool).unwrap_or(out.solo);
        out.volume_db = meta.get("volume_db").and_then(serde_json::Value::as_f64).unwrap_or(out.volume_db);
        out.pan = meta.get("pan").and_then(serde_json::Value::as_f64).unwrap_or(out.pan);
        if let Some(effects) = meta.get("effects").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            out.effects = effects;
        }
    }
    out.id = film::TrackId(t.id.0);
    out.kind = kind;
    out.name = t.name.clone();
    out.locked = t.locked;
    out.enabled = t.enabled;
    out.channels = from_channels(t.channels);
    out.items = t.items.iter().map(|item| from_track_item(item, kind, builder)).collect::<Result<_, _>>()?;
    out.transitions = t.transitions.iter().map(from_transition).collect::<Result<_, _>>()?;
    Ok(out)
}
fn from_sequence(seq: &shared::Sequence, ext: &shared::Ext, builder: &mut crate::common::Builder) -> Result<film::Sequence, serde_json::Error> {
    let mut out = crate::common::empty_sequence(from_settings(&seq.settings));
    if let Some(meta) = legacy_or_ext(ext) {
        if let Some(settings) = meta.get("filmcraft_settings").or_else(|| meta.get("settings")).and_then(|v| serde_json::from_value(v.clone()).ok()) {
            out.settings = settings;
        }
        out.master_volume_db = meta.get("master_volume_db").and_then(serde_json::Value::as_f64).unwrap_or(out.master_volume_db);
        if let Some(effects) = meta.get("master_effects").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            out.master_effects = effects;
        }
    }
    out.settings.width = seq.settings.width;
    out.settings.height = seq.settings.height;
    out.settings.frame_rate = seq.settings.frame_rate;
    out.settings.par = seq.settings.par;
    out.settings.sample_rate = seq.settings.sample_rate;
    out.settings.drop_frame = seq.settings.drop_frame;
    out.video_tracks = seq.video_tracks.iter().map(|t| from_track(t, builder)).collect::<Result<_, _>>()?;
    out.audio_tracks = seq.audio_tracks.iter().map(|t| from_track(t, builder)).collect::<Result<_, _>>()?;
    out.markers = seq.markers.iter().map(from_marker).collect::<Result<_, _>>()?;
    out.start_timecode = seq.start_timecode;
    Ok(out)
}

fn to_generator(g: &filmcraft_media::Generator) -> Result<shared::Generator, serde_json::Error> {
    use filmcraft_media::Generator as F;
    Ok(match g {
        F::BarsAndTone => shared::Generator::BarsAndTone,
        F::BlackVideo => shared::Generator::BlackVideo,
        F::TransparentVideo => shared::Generator::TransparentVideo,
        F::CountingLeader => shared::Generator::CountingLeader,
        F::ColorMatte { color } => shared::Generator::ColorMatte { color: *color },
        _ => shared::Generator::Other { name: g.label(), data: serde_json::to_string(g)? },
    })
}
fn from_generator(g: &shared::Generator) -> filmcraft_media::Generator {
    use filmcraft_media::Generator as F;
    match g {
        shared::Generator::BarsAndTone => F::BarsAndTone,
        shared::Generator::BlackVideo => F::BlackVideo,
        shared::Generator::TransparentVideo => F::TransparentVideo,
        shared::Generator::CountingLeader => F::CountingLeader,
        shared::Generator::ColorMatte { color } => F::ColorMatte { color: *color },
        shared::Generator::Other { data, .. } => serde_json::from_str(data).unwrap_or(F::BlackVideo),
    }
}
fn to_media(m: &film::MediaClip) -> Result<shared::MediaClip, serde_json::Error> {
    use filmcraft_media::MediaKind as F;
    let info = &m.info;
    let kind = match info.kind {
        F::Movie => shared::MediaKind::Movie,
        F::AudioOnly => shared::MediaKind::AudioOnly,
        F::Still => shared::MediaKind::Still,
        F::ImageSequence => shared::MediaKind::ImageSequence,
        F::Synthetic => shared::MediaKind::Synthetic,
    };
    let media = match &m.media {
        film::MediaRef::File { path } => shared::MediaRef::File { path: path.clone() },
        film::MediaRef::Generator(g) => shared::MediaRef::Generator(to_generator(g)?),
    };
    Ok(shared::MediaClip {
        media,
        info: shared::MediaInfo {
            name: info.name.clone(),
            kind,
            duration: info.duration,
            video: info.video.as_ref().map(|v| shared::VideoStreamInfo { width: v.width, height: v.height, frame_rate: v.frame_rate, par: v.par }),
            audio_streams: info
                .audio_streams
                .iter()
                .map(|a| shared::AudioStreamInfo { sample_rate: a.sample_rate, channels: a.channels, bits_per_sample: a.bits_per_sample })
                .collect(),
            start_timecode: info.start_timecode,
        },
        interpret_frame_rate: m.interpret.frame_rate,
        markers: m.markers.iter().map(to_marker).collect::<Result<_, _>>()?,
        offline: m.offline,
    })
}
fn from_media(m: &shared::MediaClip, ext: &shared::Ext) -> Result<film::MediaClip, serde_json::Error> {
    let media = match &m.media {
        shared::MediaRef::File { path } => film::MediaRef::File { path: path.clone() },
        shared::MediaRef::Generator(g) => film::MediaRef::Generator(from_generator(g)),
    };
    let spec = crate::common::MediaSpec {
        duration: Some(m.info.duration),
        video: m.info.video.as_ref().map(|v| (v.width, v.height, v.frame_rate)),
        audio: m.info.audio().map(|a| (a.sample_rate, a.channels)),
        start_tc: m.info.start_timecode,
        kind: None,
    };
    let mut out = crate::common::media_clip(media.clone(), crate::common::media_info(&m.info.name, &spec));
    if let Some(info) =
        legacy_or_ext(ext).and_then(|meta| meta.get("filmcraft_info").or_else(|| meta.get("info")).and_then(|v| serde_json::from_value(v.clone()).ok()))
    {
        out.info = info;
    }
    out.media = media;
    out.info.name = m.info.name.clone();
    out.info.duration = m.info.duration;
    out.info.kind = match m.info.kind {
        shared::MediaKind::Movie => filmcraft_media::MediaKind::Movie,
        shared::MediaKind::AudioOnly => filmcraft_media::MediaKind::AudioOnly,
        shared::MediaKind::Still => filmcraft_media::MediaKind::Still,
        shared::MediaKind::ImageSequence => filmcraft_media::MediaKind::ImageSequence,
        shared::MediaKind::Synthetic => filmcraft_media::MediaKind::Synthetic,
    };
    if let Some(source) = &m.info.video {
        if out.info.video.is_none() {
            out.info.video = crate::common::media_info(&m.info.name, &spec).video;
        }
        if let Some(v) = &mut out.info.video {
            v.width = source.width;
            v.height = source.height;
            v.frame_rate = source.frame_rate;
            v.par = source.par;
        }
    } else {
        out.info.video = None;
    }
    out.info.audio_streams = m
        .info
        .audio_streams
        .iter()
        .enumerate()
        .map(|(index, a)| {
            let mut out = out.info.audio_streams.get(index).cloned().unwrap_or(filmcraft_media::AudioStreamInfo {
                sample_rate: a.sample_rate,
                channels: a.channels,
                codec: String::new(),
                bits_per_sample: a.bits_per_sample,
            });
            out.sample_rate = a.sample_rate;
            out.channels = a.channels;
            out.bits_per_sample = a.bits_per_sample;
            out
        })
        .collect();
    out.info.start_timecode = m.info.start_timecode;
    out.interpret.frame_rate = m.interpret_frame_rate;
    out.offline = m.offline;
    out.markers = m.markers.iter().map(from_marker).collect::<Result<_, _>>()?;
    Ok(out)
}

fn from_bin(bin: &shared::Bin) -> film::Bin {
    film::Bin {
        id: film::BinId(bin.id.0),
        name: bin.name.clone(),
        children: bin
            .children
            .iter()
            .map(|e| match e {
                shared::BinEntry::Item(id) => film::BinEntry::Item(film::ItemId(id.0)),
                shared::BinEntry::Bin(b) => film::BinEntry::Bin(from_bin(b)),
            })
            .collect(),
    }
}
fn to_project(project: &film::Project, root: film::ItemId) -> Result<shared::Project, serde_json::Error> {
    let mut out = shared::Project::new(&project.name);
    out.next_id = project.next_id;
    let mut pending = vec![root];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(item) = project.items.get(&id) else { continue };
        match &item.kind {
            film::ItemKind::Sequence(seq) => pending.extend(seq.all_tracks().flat_map(|track| track.items.iter().map(|clip| clip.item))),
            film::ItemKind::Subclip { parent, .. } => pending.push(*parent),
            _ => {}
        }
        let (kind, ext) = match &item.kind {
            film::ItemKind::Media(m) => {
                (shared::ItemKind::Media(Box::new(to_media(m)?)), serde_json::json!({"filmcraft_info":m.info,"custom_label_color":item.custom_label_color}))
            }
            film::ItemKind::Sequence(seq) => (
                shared::ItemKind::Sequence(Box::new(to_sequence(seq)?)),
                serde_json::json!({"filmcraft_settings":seq.settings,"custom_label_color":item.custom_label_color,"master_volume_db":seq.master_volume_db,"master_effects":seq.master_effects,"custom_colors":project.custom_colors,"custom_color_names":project.custom_color_names}),
            ),
            film::ItemKind::Subclip { parent, range, .. } => {
                (shared::ItemKind::Subclip { parent: shared::ItemId(parent.0), range: *range }, serde_json::json!({}))
            }
            film::ItemKind::AdjustmentLayer { width, height, rate, duration } => (
                shared::ItemKind::Uncarried { what: "adjustment layer".into(), width: *width, height: *height, rate: *rate, duration: *duration },
                serde_json::json!({}),
            ),
            film::ItemKind::Graphic { width, height, rate } => (
                shared::ItemKind::Uncarried { what: "graphic clip".into(), width: *width, height: *height, rate: *rate, duration: Tick::ZERO },
                serde_json::json!({}),
            ),
        };
        out.items.insert(
            shared::ItemId(id.0),
            shared::ProjectItem {
                id: shared::ItemId(id.0),
                name: item.name.clone(),
                label: shared::Label::from_name(item.label.name()).unwrap_or(shared::Label::Iris),
                kind,
                metadata: item.metadata.clone(),
                ext: Some(ext.to_string()),
            },
        );
    }
    Ok(out)
}
fn from_project(project: &shared::Project) -> Result<film::Project, serde_json::Error> {
    let mut builder = crate::common::Builder::new(&project.name);
    let mut out = film::Project::new(&project.name);
    out.root = from_bin(&project.root);
    out.next_id = project.next_id;
    for (id, item) in &project.items {
        let kind = match &item.kind {
            shared::ItemKind::Media(m) => film::ItemKind::Media(Box::new(from_media(m, &item.ext)?)),
            shared::ItemKind::Sequence(seq) => film::ItemKind::Sequence(std::sync::Arc::new(from_sequence(seq, &item.ext, &mut builder)?)),
            shared::ItemKind::Subclip { parent, range } => film::ItemKind::Subclip { parent: film::ItemId(parent.0), range: *range, restrict_trims: false },
            shared::ItemKind::Uncarried { width, height, rate, duration, .. } => {
                film::ItemKind::AdjustmentLayer { width: *width, height: *height, rate: *rate, duration: *duration }
            }
        };
        let mut out_item = film::ProjectItem {
            id: film::ItemId(id.0),
            name: item.name.clone(),
            label: film::Label::from_name(item.label.name()).unwrap_or(film::Label::Iris),
            custom_label_color: None,
            kind,
            metadata: item.metadata.clone(),
            created: 0,
            split: Default::default(),
        };
        if let Some(meta) = legacy_or_ext(&item.ext) {
            out_item.custom_label_color = meta.get("custom_label_color").and_then(|v| serde_json::from_value(v.clone()).ok());
        }
        out.items.insert(out_item.id, out_item);
    }
    out.next_id = out.next_id.max(builder.p.next_id);
    Ok(out)
}

struct FilmCatalog;
impl FilmCatalog {
    fn convert(def: &film::effect::EffectDef) -> Option<shared::EffectDef> {
        use film::effect::EffectKind as F;
        let kind = match def.kind {
            F::Video => shared::EffectKind::Video,
            F::Audio => shared::EffectKind::Audio,
            F::VideoTransition => shared::EffectKind::VideoTransition,
            F::AudioTransition => shared::EffectKind::AudioTransition,
        };
        let params = def
            .params
            .iter()
            .map(|p| Some(shared::ParamDef { id: p.id.into(), label: p.label.into(), default: to_value(&p.default).ok()? }))
            .collect::<Option<Vec<_>>>()?;
        Some(shared::EffectDef {
            id: def.id.into(),
            name: film::effect::interchange_name(def).into(),
            kind,
            category: def.category.last().copied().unwrap_or("").into(),
            intrinsic: def.intrinsic,
            params,
        })
    }
}
impl shared::Catalog for FilmCatalog {
    fn effect(&self, id: &str) -> Option<shared::EffectDef> {
        film::find_effect(id).and_then(Self::convert)
    }
    fn effect_by_name(&self, name: &str) -> Option<shared::EffectDef> {
        film::effect::find_effect_by_name(name).and_then(Self::convert)
    }
    fn generator_by_name(&self, name: &str) -> Option<shared::Generator> {
        let scene = filmcraft_media::DemoScene::ALL.iter().find(|scene| scene.file_name().eq_ignore_ascii_case(name))?;
        to_generator(&filmcraft_media::Generator::Demo(*scene)).ok()
    }
    fn intrinsic(&self, kind: shared::TrackKind) -> Vec<shared::EffectInstance> {
        let values = match kind {
            shared::TrackKind::Video => film::effect::intrinsic_video(),
            shared::TrackKind::Audio => film::effect::intrinsic_audio(),
        };
        values.iter().filter_map(|v| to_effect(v).ok()).collect()
    }
}

fn preserve_legacy_metadata(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(meta) = map.get_mut("metadata").and_then(|v| v.get_mut("filmcraft")).and_then(serde_json::Value::as_object_mut) {
                // Keep the old application's object before the shared reader consumes known keys.
                let mut original = meta.clone();
                original.remove("filmcraft_legacy_metadata");
                meta.insert("filmcraft_legacy_metadata".into(), serde_json::Value::Object(original));
            }
            for (key, child) in map.iter_mut() {
                if key != "metadata" {
                    preserve_legacy_metadata(child);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                preserve_legacy_metadata(child);
            }
        }
        _ => {}
    }
}
fn restore_legacy_output(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(meta) = map.get_mut("metadata").and_then(|v| v.get_mut("filmcraft")).and_then(serde_json::Value::as_object_mut) {
                if let Some(settings) = meta.remove("filmcraft_settings") {
                    meta.insert("settings".into(), settings);
                }
                if let Some(info) = meta.remove("filmcraft_info") {
                    meta.insert("info".into(), info);
                }
            }
            for (key, child) in map.iter_mut() {
                if key != "metadata" {
                    restore_legacy_output(child);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                restore_legacy_output(child);
            }
        }
        _ => {}
    }
}
fn legacy_or_ext(ext: &shared::Ext) -> Option<serde_json::Value> {
    let outer: serde_json::Value = serde_json::from_str(ext.as_deref()?).ok()?;
    Some(outer.get("filmcraft_legacy_metadata").cloned().unwrap_or(outer))
}
fn merge_report(dst: &mut crate::Report, src: craft_timeline::Report) {
    for entry in src.entries {
        let level = match entry.level {
            craft_timeline::Level::Info => crate::Level::Info,
            craft_timeline::Level::Warning => crate::Level::Warning,
        };
        for _ in 0..entry.count {
            dst.push(level, entry.message.clone());
        }
    }
}
fn convert_error(e: craft_timeline::Error) -> crate::Error {
    match e {
        craft_timeline::Error::Parse { format, message } => crate::Error::Parse { format, message },
        craft_timeline::Error::Empty => crate::Error::Empty,
        craft_timeline::Error::NoSequence(id) => crate::Error::NoSequence(film::ItemId(id.0)),
        craft_timeline::Error::Other(message) => crate::Error::Other(message),
    }
}
fn merge_otio_palette(project: &mut film::Project, meta: Option<&serde_json::Value>, report: &mut crate::Report) {
    use serde_json::Value;
    if let Some(colors) = meta.and_then(|m| m.get("custom_colors")).and_then(Value::as_array) {
        let mut overflow = false;
        for color in colors.iter().filter_map(Value::as_str).filter_map(filmcraft_project::HexColor::parse) {
            if project.custom_colors.contains(&color) {
                continue;
            }
            if project.custom_colors.len() >= 64 {
                overflow = true;
                break;
            }
            project.custom_colors.push(color);
        }
        if overflow {
            report.warn("OTIO saved palettes exceed the 64-color project limit; additional colors were not imported.");
        }
    }
    if let Some(names) = meta.and_then(|m| m.get("custom_color_names")).and_then(Value::as_object) {
        for (key, value) in names {
            if let (Some(color), Some(name)) = (filmcraft_project::HexColor::parse(key), value.as_str())
                && project.custom_colors.contains(&color)
                && name.len() <= 128
                && !name.chars().any(char::is_control)
            {
                project.custom_color_names.entry(color).or_insert_with(|| name.to_owned());
            }
        }
    }
}
pub(crate) fn import_otio(text: &str, opts: &crate::ImportOptions, report: &mut crate::Report) -> crate::Result<crate::Imported> {
    let mut value: serde_json::Value = serde_json::from_str(text).map_err(|e| crate::Error::parse(crate::Format::Otio, e.to_string()))?;
    preserve_legacy_metadata(&mut value);
    let normalized = serde_json::to_string(&value).map_err(|e| crate::Error::Other(e.to_string()))?;
    drop(value);
    let catalog = FilmCatalog;
    let shared_opts =
        craft_timeline::ImportOptions { base_dir: opts.base_dir.as_deref(), name: opts.name.as_deref(), catalog: &catalog, namespace: "filmcraft" };
    let mut shared_report = craft_timeline::Report::default();
    let result = craft_timeline::otio::import_with_report_limit(&normalized, &shared_opts, &mut shared_report, Some(crate::MAX_REPORT_ENTRIES));
    merge_report(report, shared_report);
    let imported = result.map_err(convert_error)?;
    let mut project = from_project(&imported.project).map_err(|e| crate::Error::Other(e.to_string()))?;
    for id in &imported.sequences {
        merge_otio_palette(&mut project, imported.project.items.get(id).and_then(|item| legacy_or_ext(&item.ext)).as_ref(), report);
    }
    Ok(crate::Imported { project, sequences: imported.sequences.into_iter().map(|id| film::ItemId(id.0)).collect() })
}
pub(crate) fn export_otio(project: &film::Project, seq: film::ItemId, opts: &crate::ExportOptions, report: &mut crate::Report) -> crate::Result<String> {
    let project = to_project(project, seq).map_err(|e| crate::Error::Other(e.to_string()))?;
    let catalog = FilmCatalog;
    let shared_opts = craft_timeline::ExportOptions {
        relative_to: opts.relative_to.as_deref(),
        name: opts.name.as_deref(),
        catalog: &catalog,
        namespace: "filmcraft",
        xmeml_version: opts.xmeml_version,
        fcpxml_version: Default::default(),
    };
    let mut shared_report = craft_timeline::Report::default();
    let result =
        craft_timeline::otio::export_with_report_limit(&project, shared::ItemId(seq.0), &shared_opts, &mut shared_report, Some(crate::MAX_REPORT_ENTRIES));
    merge_report(report, shared_report);
    let text = result.map_err(convert_error)?;
    let mut value: serde_json::Value = serde_json::from_str(&text).map_err(|e| crate::Error::Other(e.to_string()))?;
    restore_legacy_output(&mut value);
    serde_json::to_string_pretty(&value).map_err(|e| crate::Error::Other(e.to_string()))
}

pub(crate) fn import_fcpxml(text: &str, opts: &crate::ImportOptions, report: &mut crate::Report) -> crate::Result<crate::Imported> {
    let catalog = FilmCatalog;
    let shared_opts =
        craft_timeline::ImportOptions { base_dir: opts.base_dir.as_deref(), name: opts.name.as_deref(), catalog: &catalog, namespace: "filmcraft" };
    let mut shared_report = craft_timeline::Report::default();
    let result = craft_timeline::fcpxml::import_with_report_limit(text, &shared_opts, &mut shared_report, Some(crate::MAX_REPORT_ENTRIES));
    merge_report(report, shared_report);
    let imported = result.map_err(convert_error)?;
    let project = from_project(&imported.project).map_err(|e| crate::Error::Other(e.to_string()))?;
    Ok(crate::Imported { project, sequences: imported.sequences.into_iter().map(|id| film::ItemId(id.0)).collect() })
}
pub(crate) fn export_fcpxml(project: &film::Project, seq: film::ItemId, opts: &crate::ExportOptions, report: &mut crate::Report) -> crate::Result<String> {
    crate::warn_marker_metadata_loss(project, seq, report);
    let project = to_project(project, seq).map_err(|e| crate::Error::Other(e.to_string()))?;
    let catalog = FilmCatalog;
    let version = match opts.fcpxml_version {
        crate::FcpxmlVersion::V1_9 => craft_timeline::FcpxmlVersion::V1_9,
        crate::FcpxmlVersion::V1_10 => craft_timeline::FcpxmlVersion::V1_10,
        crate::FcpxmlVersion::V1_11 => craft_timeline::FcpxmlVersion::V1_11,
    };
    let shared_opts = craft_timeline::ExportOptions {
        relative_to: opts.relative_to.as_deref(),
        name: opts.name.as_deref(),
        catalog: &catalog,
        namespace: "filmcraft",
        xmeml_version: opts.xmeml_version,
        fcpxml_version: version,
    };
    let mut shared_report = craft_timeline::Report::default();
    let result =
        craft_timeline::fcpxml::export_with_report_limit(&project, shared::ItemId(seq.0), &shared_opts, &mut shared_report, Some(crate::MAX_REPORT_ENTRIES));
    merge_report(report, shared_report);
    result.map_err(convert_error)
}

pub(crate) fn import_fcp7(text: &str, opts: &crate::ImportOptions, report: &mut crate::Report) -> crate::Result<crate::Imported> {
    let catalog = FilmCatalog;
    let shared_opts =
        craft_timeline::ImportOptions { base_dir: opts.base_dir.as_deref(), name: opts.name.as_deref(), catalog: &catalog, namespace: "filmcraft" };
    let mut shared_report = craft_timeline::Report::default();
    let result = craft_timeline::fcp7::import_with_report_limit(text, &shared_opts, &mut shared_report, Some(crate::MAX_REPORT_ENTRIES));
    merge_report(report, shared_report);
    let imported = result.map_err(convert_error)?;
    let project = from_project(&imported.project).map_err(|e| crate::Error::Other(e.to_string()))?;
    Ok(crate::Imported { project, sequences: imported.sequences.into_iter().map(|id| film::ItemId(id.0)).collect() })
}
pub(crate) fn export_fcp7(project: &film::Project, seq: film::ItemId, opts: &crate::ExportOptions, report: &mut crate::Report) -> crate::Result<String> {
    let project = to_project(project, seq).map_err(|e| crate::Error::Other(e.to_string()))?;
    let catalog = FilmCatalog;
    let shared_opts = craft_timeline::ExportOptions {
        relative_to: opts.relative_to.as_deref(),
        name: opts.name.as_deref(),
        catalog: &catalog,
        namespace: "filmcraft",
        xmeml_version: opts.xmeml_version,
        fcpxml_version: Default::default(),
    };
    let mut shared_report = craft_timeline::Report::default();
    let result =
        craft_timeline::fcp7::export_with_report_limit(&project, shared::ItemId(seq.0), &shared_opts, &mut shared_report, Some(crate::MAX_REPORT_ENTRIES));
    merge_report(report, shared_report);
    result.map_err(convert_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effect_keyframes_and_app_fields_roundtrip() {
        let mut effect = film::EffectInstance {
            effect: "unknown".into(),
            enabled: true,
            params: Default::default(),
            masks: Vec::new(),
            post_fader: false,
            essential: false,
            layer: None,
        };
        effect.post_fader = true;
        let mut param = film::Param::new(film::ParamValue::Curve(vec![[0.25, 0.75]]));
        let mut key = film::Keyframe::new(Tick(123), film::ParamValue::Vec2(filmcraft_geom::Vec2::new(1.0, 2.0)));
        key.interp = film::Interpolation::Bezier;
        key.out_influence = 0.6;
        param.keyframes.push(key);
        effect.params.insert("curve".into(), param);
        let neutral = to_effect(&effect).unwrap();
        assert_eq!(from_effect(&neutral).unwrap(), effect);
    }
    #[test]
    fn shared_reader_matches_legacy_plain_clip_timing() {
        let doc = r#"{"OTIO_SCHEMA":"Timeline.1","name":"T","tracks":{"OTIO_SCHEMA":"Stack.1","children":[{"OTIO_SCHEMA":"Track.1","kind":"Video","children":[{"OTIO_SCHEMA":"Clip.2","name":"A","source_range":{"OTIO_SCHEMA":"TimeRange.1","start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":24,"value":12},"duration":{"OTIO_SCHEMA":"RationalTime.1","rate":24,"value":48}},"media_references":{"DEFAULT_MEDIA":{"OTIO_SCHEMA":"ExternalReference.1","target_url":"file:///tmp/a.mov"}},"active_media_reference_key":"DEFAULT_MEDIA"}]}]}}"#;
        let opts = crate::ImportOptions::default();
        let old = crate::otio::import(doc, &opts, &mut crate::Report::default()).unwrap();
        let new = import_otio(doc, &opts, &mut crate::Report::default()).unwrap();
        let old_clip = &old.project.sequence(old.sequences[0]).unwrap().video_tracks[0].items[0];
        let new_clip = &new.project.sequence(new.sequences[0]).unwrap().video_tracks[0].items[0];
        assert_eq!((new_clip.start, new_clip.duration, new_clip.source_in), (old_clip.start, old_clip.duration, old_clip.source_in));
    }
    #[test]
    fn shared_fcpxml_reader_matches_legacy_plain_clip_timing() {
        let doc = r#"<fcpxml version="1.10"><resources><format id="r1" frameDuration="1/24s" width="1920" height="1080"/><asset id="r2" name="A" hasVideo="1" duration="4s"><media-rep kind="original-media" src="file:///tmp/a.mov"/></asset></resources><library><event><project name="P"><sequence format="r1" duration="4s"><spine><asset-clip ref="r2" name="A" offset="0s" start="1s" duration="2s"/></spine></sequence></project></event></library></fcpxml>"#;
        let opts = crate::ImportOptions::default();
        let old = crate::fcpxml::import(doc, &opts, &mut crate::Report::default()).unwrap();
        let new = import_fcpxml(doc, &opts, &mut crate::Report::default()).unwrap();
        let old_clip = &old.project.sequence(old.sequences[0]).unwrap().video_tracks[0].items[0];
        let new_clip = &new.project.sequence(new.sequences[0]).unwrap().video_tracks[0].items[0];
        assert_eq!((new_clip.start, new_clip.duration, new_clip.source_in), (old_clip.start, old_clip.duration, old_clip.source_in));
    }
    #[test]
    fn shared_fcp7_reader_matches_legacy_plain_clip_timing() {
        let doc = r#"<xmeml version="4"><sequence id="s"><name>S</name><rate><timebase>24</timebase></rate><media><video><track><clipitem id="c"><name>A</name><start>0</start><end>48</end><in>12</in><out>60</out><file id="f"><name>a.mov</name><pathurl>file:///tmp/a.mov</pathurl><media><video/></media></file></clipitem></track></video></media></sequence></xmeml>"#;
        let opts = crate::ImportOptions::default();
        let old = crate::fcp7::import(doc, &opts, &mut crate::Report::default()).unwrap();
        let new = import_fcp7(doc, &opts, &mut crate::Report::default()).unwrap();
        let old_clip = &old.project.sequence(old.sequences[0]).unwrap().video_tracks[0].items[0];
        let new_clip = &new.project.sequence(new.sequences[0]).unwrap().video_tracks[0].items[0];
        assert_eq!((new_clip.start, new_clip.duration, new_clip.source_in), (old_clip.start, old_clip.duration, old_clip.source_in));
    }
    #[test]
    fn exported_metadata_keeps_legacy_settings_and_media_info() {
        let mut value = serde_json::json!({"metadata":{"filmcraft":{"settings":{"width":1},"filmcraft_settings":{"width":2,"preset":"legacy"},"info":{"name":"shared"},"filmcraft_info":{"name":"original","container":"mov"}}}});
        restore_legacy_output(&mut value);
        let meta = &value["metadata"]["filmcraft"];
        assert_eq!(meta["settings"]["preset"], "legacy");
        assert_eq!(meta["info"]["container"], "mov");
        assert!(meta.get("filmcraft_settings").is_none() && meta.get("filmcraft_info").is_none());
    }
}

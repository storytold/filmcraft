//! Conversion between the project-facing composition and the shared OMF model.
use crate::comp as local;
use craft_aaf::model as shared;
use craft_aaf::time as st;
use filmcraft_time as lt;

fn s_tick(t: lt::Tick) -> st::Tick {
    st::Tick(t.0)
}
fn l_tick(t: st::Tick) -> lt::Tick {
    lt::Tick(t.0)
}
fn s_rate(r: lt::FrameRate) -> st::FrameRate {
    st::FrameRate { num: r.num, den: r.den }
}
fn l_rate(r: st::FrameRate) -> lt::FrameRate {
    lt::FrameRate { num: r.num, den: r.den }
}
fn s_kind(k: local::CKind) -> shared::CKind {
    match k {
        local::CKind::Picture => shared::CKind::Picture,
        local::CKind::Sound => shared::CKind::Sound,
    }
}
fn l_kind(k: shared::CKind) -> local::CKind {
    match k {
        shared::CKind::Picture => local::CKind::Picture,
        shared::CKind::Sound => local::CKind::Sound,
    }
}
fn s_gain(g: local::Gain) -> shared::Gain {
    match g {
        local::Gain::Constant(v) => shared::Gain::Constant(v),
        local::Gain::Varying { linear, points } => shared::Gain::Varying { linear, points: points.into_iter().map(|(t, v)| (s_tick(t), v)).collect() },
    }
}
fn l_gain(g: shared::Gain) -> local::Gain {
    match g {
        shared::Gain::Constant(v) => local::Gain::Constant(v),
        shared::Gain::Varying { linear, points } => local::Gain::Varying { linear, points: points.into_iter().map(|(t, v)| (l_tick(t), v)).collect() },
    }
}
fn s_item(i: local::CItem) -> shared::CItem {
    match i {
        local::CItem::Filler(t) => shared::CItem::Filler(s_tick(t)),
        local::CItem::Clip(c) => {
            shared::CItem::Clip(shared::CClip { len: s_tick(c.len), source: c.source, start: s_tick(c.start), gain: c.gain.map(s_gain), name: c.name })
        }
        local::CItem::Transition(t) => shared::CItem::Transition(shared::CTransition { len: s_tick(t.len), cut: s_tick(t.cut), effect: t.effect }),
    }
}
fn l_item(i: shared::CItem) -> local::CItem {
    match i {
        shared::CItem::Filler(t) => local::CItem::Filler(l_tick(t)),
        shared::CItem::Clip(c) => {
            local::CItem::Clip(local::CClip { len: l_tick(c.len), source: c.source, start: l_tick(c.start), gain: c.gain.map(l_gain), name: c.name })
        }
        shared::CItem::Transition(t) => local::CItem::Transition(local::CTransition { len: l_tick(t.len), cut: l_tick(t.cut), effect: t.effect }),
    }
}
fn s_marker(m: local::CMarker) -> shared::CMarker {
    shared::CMarker { start: s_tick(m.start), duration: s_tick(m.duration), name: m.name, comment: m.comment, color: m.color }
}
fn l_marker(m: shared::CMarker) -> local::CMarker {
    local::CMarker { start: l_tick(m.start), duration: l_tick(m.duration), name: m.name, comment: m.comment, color: m.color }
}
fn s_comp(c: local::Composition) -> shared::Composition {
    shared::Composition {
        name: c.name,
        aaf_mob_id: None,
        rate: s_rate(c.rate),
        sample_rate: c.sample_rate,
        width: c.width,
        height: c.height,
        start_tc: c.start_tc,
        drop: c.drop,
        tracks: c
            .tracks
            .into_iter()
            .map(|t| shared::CTrack {
                kind: s_kind(t.kind),
                name: t.name,
                number: t.number,
                channels: t.channels,
                items: t.items.into_iter().map(s_item).collect(),
            })
            .collect(),
        markers: c.markers.into_iter().map(s_marker).collect(),
    }
}
fn l_comp(c: shared::Composition) -> local::Composition {
    local::Composition {
        name: c.name,
        rate: l_rate(c.rate),
        sample_rate: c.sample_rate,
        width: c.width,
        height: c.height,
        start_tc: c.start_tc,
        drop: c.drop,
        tracks: c
            .tracks
            .into_iter()
            .map(|t| local::CTrack {
                kind: l_kind(t.kind),
                name: t.name,
                number: t.number,
                channels: t.channels,
                items: t.items.into_iter().map(l_item).collect(),
            })
            .collect(),
        markers: c.markers.into_iter().map(l_marker).collect(),
    }
}
fn s_source(s: local::Source) -> shared::Source {
    shared::Source {
        key: s.key,
        name: s.name,
        kind: s_kind(s.kind),
        path: s.path,
        channel: s.channel,
        channels: s.channels,
        file_channels: s.file_channels,
        width: s.width,
        height: s.height,
        frame_rate: s_rate(s.frame_rate),
        sample_rate: s.sample_rate,
        bits: s.bits,
        length: s_tick(s.length),
        offset: s_tick(s.offset),
        start_tc: s.start_tc,
        tc_rate: s_rate(s.tc_rate),
        embedded: s.embedded,
        markers: s.markers.into_iter().map(s_marker).collect(),
        nested: s.nested,
    }
}
fn l_source(s: shared::Source) -> local::Source {
    local::Source {
        key: s.key,
        name: s.name,
        kind: l_kind(s.kind),
        path: s.path,
        channel: s.channel,
        channels: s.channels,
        file_channels: s.file_channels,
        width: s.width,
        height: s.height,
        frame_rate: l_rate(s.frame_rate),
        sample_rate: s.sample_rate,
        bits: s.bits,
        length: l_tick(s.length),
        offset: l_tick(s.offset),
        start_tc: s.start_tc,
        tc_rate: l_rate(s.tc_rate),
        embedded: s.embedded,
        markers: s.markers.into_iter().map(l_marker).collect(),
        nested: s.nested,
    }
}
pub(crate) fn to_shared(d: local::Document) -> shared::Document {
    shared::Document {
        compositions: d.compositions.into_iter().map(s_comp).collect(),
        sources: d.sources.into_iter().map(s_source).collect(),
        nested: d.nested.into_iter().map(s_comp).collect(),
    }
}
pub(crate) fn from_shared(d: shared::Document) -> local::Document {
    local::Document {
        compositions: d.compositions.into_iter().map(l_comp).collect(),
        sources: d.sources.into_iter().map(l_source).collect(),
        nested: d.nested.into_iter().map(l_comp).collect(),
    }
}
pub(crate) fn error(e: craft_aaf::Error) -> crate::Error {
    match e {
        craft_aaf::Error::Parse { format, message } => crate::Error::Parse { format, message },
        craft_aaf::Error::Empty => crate::Error::Empty,
        craft_aaf::Error::Other(s) => crate::Error::Other(s),
    }
}
pub(crate) fn merge_report(dst: &mut crate::Report, src: craft_aaf::Report) {
    for e in src.entries {
        let level = match e.level {
            craft_aaf::report::Level::Info => crate::Level::Info,
            craft_aaf::report::Level::Warning => crate::Level::Warning,
        };
        let key = if dst.entries.len() >= crate::MAX_REPORT_ENTRIES && !dst.entries.iter().any(|x| x.level == level && x.message == e.message) {
            "further diagnostics omitted"
        } else {
            &e.message
        };
        if let Some(existing) = dst.entries.iter_mut().find(|x| x.level == level && x.message == key) {
            existing.count = existing.count.saturating_add(e.count);
        } else {
            dst.entries.push(crate::ReportEntry { level, message: key.to_string(), count: e.count });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_mapping_keeps_timing_and_items() {
        let original = local::Document {
            compositions: vec![local::Composition {
                name: "Cut".into(),
                rate: lt::FrameRate::FPS_29_97,
                sample_rate: 48_000,
                width: 1920,
                height: 1080,
                start_tc: 42,
                drop: true,
                tracks: vec![local::CTrack {
                    kind: local::CKind::Sound,
                    name: "A1".into(),
                    number: 1,
                    channels: 2,
                    items: vec![local::CItem::Clip(local::CClip {
                        len: lt::Tick(123),
                        source: 2,
                        start: lt::Tick(45),
                        gain: Some(local::Gain::Varying { linear: true, points: vec![(lt::Tick(7), 0.5)] }),
                        name: "Clip".into(),
                    })],
                }],
                markers: vec![local::CMarker { start: lt::Tick(8), duration: lt::Tick(9), name: "M".into(), comment: "C".into(), color: Some("blue".into()) }],
            }],
            ..local::Document::default()
        };
        assert_eq!(from_shared(to_shared(original.clone())), original);
    }
}

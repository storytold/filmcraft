#![allow(clippy::option_map_unit_fn)]
//! A ready-made demo project (synthetic footage, a cut sequence with transitions, titles-free
//! effects, markers and music) so the editor shows a realistic workspace with no files at all.

use std::sync::Arc;

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{
    BinId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, TrackKind, Transition,
    TransitionId, find_effect, resolve_auto_points,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use crate::MediaPool;

/// Add a generator-backed media item to the project and the pool.
pub fn add_generator(p: &mut Project, pool: &MediaPool, src: GeneratorSource, name: &str, label: Label, bin: Option<BinId>) -> ItemId {
    let info = {
        let mut i = src.info().clone();
        i.name = name.to_string();
        i
    };
    let clip = MediaClip {
        media: MediaRef::Generator(src.generator.clone()),
        info,
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let key = crate::media_pool::media_key(&clip);
    let id = p.add_item(name, label, ItemKind::Media(clip), bin);
    pool.insert_keyed(id, key, Arc::new(src.with_name(name)));
    id
}

/// Build the demo project. Returns (project, main sequence).
pub fn demo_project(pool: &MediaPool) -> (Project, ItemId) {
    build_demo(pool).unwrap_or_else(|| {
        // Only reachable if the built-in items could not be placed: an empty demo, not a crash.
        let mut p = Project::new("FilmCraft Demo");
        let seq = p.new_sequence("Main Edit", SequenceSettings::default(), 3, 3, None);
        (p, seq)
    })
}

fn build_demo(pool: &MediaPool) -> Option<(Project, ItemId)> {
    let mut p = Project::new("FilmCraft Demo");
    let footage = p.add_bin("Footage", None);
    let audio_bin = p.add_bin("Audio", None);
    let gfx = p.add_bin("Graphics", None);
    let seqs = p.add_bin("Sequences", None);

    let mut clips = Vec::new();
    for s in DemoScene::ALL {
        let src = GeneratorSource::demo(s);
        let id = add_generator(&mut p, pool, src, s.file_name(), Label::Iris, Some(footage));
        clips.push(id);
    }
    let music = add_generator(
        &mut p,
        pool,
        GeneratorSource::new(Generator::Demo(DemoScene::Aurora), 1920, 1080, FrameRate::FPS_23_976, Tick(60 * TICKS_PER_SECOND)),
        "Ambient_Score.wav",
        Label::Caribbean,
        Some(audio_bin),
    );
    // make the "music" item audio-only in its info
    if let Some(ItemKind::Media(m)) = p.item_mut(music).map(|i| &mut i.kind) {
        m.info.video = None;
        m.info.kind = filmcraft_media::MediaKind::AudioOnly;
        m.info.container = "WAV".into();
    }
    let bars = add_generator(
        &mut p,
        pool,
        GeneratorSource::new(Generator::BarsAndTone, 1920, 1080, FrameRate::FPS_23_976, Tick(10 * TICKS_PER_SECOND)),
        "Bars and Tone",
        Label::Lavender,
        Some(gfx),
    );
    let leader = add_generator(
        &mut p,
        pool,
        GeneratorSource::new(Generator::CountingLeader, 1920, 1080, FrameRate::FPS_23_976, Tick(8 * TICKS_PER_SECOND)),
        "Universal Counting Leader",
        Label::Lavender,
        Some(gfx),
    );
    let _matte = add_generator(
        &mut p,
        pool,
        GeneratorSource::new(Generator::ColorMatte { color: [0.08, 0.1, 0.16, 1.0] }, 1920, 1080, FrameRate::FPS_23_976, Tick(5 * TICKS_PER_SECOND)),
        "Color Matte",
        Label::Lavender,
        Some(gfx),
    );
    let _ = (bars, leader);

    let settings = SequenceSettings::default();
    let r = settings.frame_rate;
    let seq = p.new_sequence("Main Edit", settings, 3, 3, Some(seqs));
    let _rough = p.new_sequence("Rough Cut", SequenceSettings::default(), 3, 2, Some(seqs));

    // V1 edit: (clip index, source in s, duration s)
    let cuts: [(usize, f64, f64); 6] = [(0, 1.0, 5.0), (1, 2.0, 4.5), (2, 0.5, 4.0), (3, 3.0, 5.0), (4, 1.0, 3.0), (5, 2.0, 5.5)];
    let mut t = Tick::ZERO;
    let mut placed = Vec::new();
    for (ci, sin, dur) in cuts {
        let src_range = TimeRange::new(r.snap(Tick::from_seconds_f64(sin)), r.snap(Tick::from_seconds_f64(dur)));
        let link = p.alloc_id();
        let mut v = p.make_track_item(clips[ci], TrackKind::Video, t, src_range, r)?;
        let mut a = p.make_track_item(clips[ci], TrackKind::Audio, t, src_range, r)?;
        v.link = Some(link);
        a.link = Some(link);
        for e in &mut v.effects {
            resolve_auto_points(e, (1920, 1080), (1920, 1080));
        }
        a.effect_mut("volume").map(|e| e.params.get_mut("level").map(|l| l.value = ParamValue::Float(-8.0)));
        let dur_t = v.duration;
        placed.push((v.id, a.id, t, dur_t));
        let s = p.sequence_mut(seq)?;
        s.video_tracks[0].items.push(v);
        s.audio_tracks[0].items.push(a);
        t += dur_t;
    }
    // A lower-third-ish overlay on V2: the plasma clip scaled down in the corner with a drop shadow.
    let ov_range = TimeRange::new(Tick::ZERO, r.snap(Tick::from_seconds_f64(4.0)));
    let mut ov = p.make_track_item(clips[4], TrackKind::Video, r.snap(Tick::from_seconds_f64(6.0)), ov_range, r)?;
    for e in &mut ov.effects {
        resolve_auto_points(e, (1920, 1080), (1920, 1080));
    }
    if let Some(m) = ov.effect_mut("motion") {
        m.params.get_mut("scale").map(|s| s.value = ParamValue::Float(32.0));
        m.params.get_mut("position").map(|s| s.value = ParamValue::Vec2(filmcraft_geom::Vec2::new(1540.0, 820.0)));
    }
    if let Some(mut ds) = find_effect("drop_shadow").map(|d| d.instance()) {
        ds.params.get_mut("distance").map(|v| v.value = ParamValue::Float(18.0));
        ds.params.get_mut("softness").map(|v| v.value = ParamValue::Float(40.0));
        ov.effects.insert(0, ds);
    }
    ov.label = Label::Mango;
    // Fade the overlay in/out with opacity keyframes (media time).
    if let Some(o) = ov.effect_mut("opacity").and_then(|o| o.params.get_mut("opacity")) {
        o.toggle_animation(Tick::ZERO);
        o.set_at(Tick::ZERO, ParamValue::Float(0.0));
        o.set_at(r.tick_of(12), ParamValue::Float(100.0));
        o.set_at(r.tick_of(84), ParamValue::Float(100.0));
        o.set_at(r.tick_of(95), ParamValue::Float(0.0));
    }
    // Grade on the dunes shot.
    if let Some(mut lum) = find_effect("lumetri").map(|d| d.instance()) {
        lum.params.get_mut("temperature").map(|v| v.value = ParamValue::Float(18.0));
        lum.params.get_mut("contrast").map(|v| v.value = ParamValue::Float(22.0));
        lum.params.get_mut("vignette_amount").map(|v| v.value = ParamValue::Float(-1.4));
        let s = p.sequence_mut(seq)?;
        if let Some(it) = s.video_tracks[0].items.get_mut(3) {
            it.effects.insert(0, lum);
        }
    }
    let s = p.sequence_mut(seq)?;
    s.video_tracks[1].items.push(ov);
    // Transitions: cross dissolve between shots 1-2, dip to black 3-4, push 5-6, fade in at start.
    let tr_len = r.tick_of(24);
    let mk = |id: u64, eff: &str, from: Option<filmcraft_project::ClipId>, to: Option<filmcraft_project::ClipId>, start: Tick| Transition {
        id: TransitionId(id),
        effect: crate::presets::effect_instance(eff),
        start,
        duration: tr_len,
        from,
        to,
        align: Default::default(),
        reverse: false,
    };
    let cut = |i: usize| placed[i].2;
    let trs = vec![
        mk(900_001, "dip_to_black", None, Some(placed[0].0), Tick::ZERO),
        mk(900_002, "cross_dissolve", Some(placed[0].0), Some(placed[1].0), cut(1) - tr_len.mul_ratio(1, 2)),
        mk(900_003, "dip_to_black", Some(placed[2].0), Some(placed[3].0), cut(3) - tr_len.mul_ratio(1, 2)),
        mk(900_004, "push", Some(placed[4].0), Some(placed[5].0), cut(5) - tr_len.mul_ratio(1, 2)),
    ];
    s.video_tracks[0].transitions = trs;
    s.audio_tracks[0].transitions.push(Transition {
        id: TransitionId(900_010),
        effect: crate::presets::effect_instance("constant_power"),
        start: cut(1) - tr_len.mul_ratio(1, 2),
        duration: tr_len,
        from: Some(placed[0].1),
        to: Some(placed[1].1),
        align: Default::default(),
        reverse: false,
    });
    let end = t;
    // Markers
    for (i, (sec, name, color)) in
        [(0.0, "Open", Label::Green), (9.5, "Music hit", Label::Rose), (18.0, "Midpoint", Label::Mango), (24.0, "Closing shot", Label::Cerulean)]
            .iter()
            .enumerate()
    {
        s.markers.push(Marker {
            id: MarkerId(800_000 + i as u64),
            start: r.snap(Tick::from_seconds_f64(*sec)),
            duration: Tick::ZERO,
            name: name.to_string(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: *color,
        });
    }
    // Music on A2 (under the whole edit), ducked -14 dB.
    let mut m = p.make_track_item(music, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, end), r)?;
    m.effect_mut("volume").map(|e| e.params.get_mut("level").map(|l| l.value = ParamValue::Float(-14.0)));
    m.label = Label::Caribbean;
    let s = p.sequence_mut(seq)?;
    s.audio_tracks[1].items.push(m);
    for tr in s.all_tracks_mut() {
        tr.sort();
    }
    // the transitions and markers above take fixed ids: new ones start past them
    p.next_id = p.next_id.max(p.max_id().saturating_add(1));
    Some((p, seq))
}

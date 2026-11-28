//! Metadata & Timecode Burn-in's duration and metadata sources (#4236) read the clip and its
//! source item through the real render environment.

use super::*;
use crate::{RenderOptions, SourceMap};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::ItemKind;
use filmcraft_project::{Label, MediaClip, MediaRef, ParamValue, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const R: FrameRate = FrameRate::FPS_24;

#[test]
fn duration_and_metadata_sources_read_the_clip_and_its_item() {
    let mut p = Project::new("burn");
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: R, ..Default::default() }, 1, 0, None);
    let g = GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.2, 0.2, 1.0] }, 320, 180, R, Tick(10 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let item = p.add_item(
        "Matte",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let it = p.item_mut(item).unwrap();
    it.metadata.insert("Description".into(), "Wide establishing".into());
    it.metadata.insert("Scene".into(), "12A".into());
    let ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, R.tick_of(48)), R).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let q = p.sequence(seq).unwrap();
    let ti = &q.video_tracks[0].items[0];
    let map = SourceMap::default();
    let env = ItemEnv {
        project: &p,
        seq: q,
        item: ti,
        t: R.tick_of(12),
        opts: RenderOptions::default(),
        sources: &map,
        want: 1.0,
        layer_size: (320, 180),
        layer_to_output: filmcraft_geom::Affine::IDENTITY,
        tc: "00:00:00:12",
    };
    let cx = FxCtx {
        t: R.tick_of(12),
        px_scale: 1.0,
        mask_scale: 1.0,
        seconds: 0.5,
        timecode: "00:00:00:12",
        clip_name: &ti.name,
        project: Some(&p),
        env: Some(&env),
        frame_rate: 24.0,
        working: q.settings.color.working,
    };
    let mut e = find_effect("metadata_burnin").unwrap().instance();
    let mut shown = |source: u32| {
        e.params.get_mut("source").unwrap().value = ParamValue::Choice(source);
        text::burnin_text(&e, &cx)
    };
    assert_eq!(shown(6), "00:00:10:00", "Media Duration: the whole source");
    assert_eq!(shown(7), "00:00:02:00", "Clip Duration: the clip on the timeline");
    assert_eq!(shown(8), "Wide establishing");
    assert_eq!(shown(9), "", "an empty Comment shows nothing");
    assert_eq!(shown(10), "12A");
}

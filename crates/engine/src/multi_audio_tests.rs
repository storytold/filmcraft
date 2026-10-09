//! A media file with several audio streams (a game track and a microphone track, say): the import
//! maps one audio clip to each stream, and placing it puts each clip on its own audio track.

use std::sync::Arc;

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{AudioStreamInfo, Generator, MediaSource};
use filmcraft_project::{AudioChannels, Interpretation, ItemKind, Label, MediaClip, MediaRef};
use filmcraft_time::{FrameRate, Tick};
use serde_json::json;

use super::*;
use crate::commands::stream_channel_map;

fn stream(channels: u32) -> AudioStreamInfo {
    AudioStreamInfo { sample_rate: 48_000, channels, codec: "AAC".into(), bits_per_sample: None }
}

#[test]
fn one_stream_keeps_the_default_mapping() {
    assert_eq!(stream_channel_map(&[]), None);
    assert_eq!(stream_channel_map(&[stream(2)]), None);
}

#[test]
fn each_stream_gets_its_own_channels_in_order() {
    let map = stream_channel_map(&[stream(2), stream(1), stream(2)]).unwrap();
    assert_eq!(map.format, AudioChannels::Stereo);
    assert_eq!(map.clips, vec![vec![0, 1], vec![2], vec![3, 4]]);
}

#[test]
fn a_surround_stream_marks_the_clips_as_surround() {
    let map = stream_channel_map(&[stream(2), stream(6)]).unwrap();
    assert_eq!(map.format, AudioChannels::Surround51);
    assert_eq!(map.clips, vec![vec![0, 1], vec![2, 3, 4, 5, 6, 7]]);
}

/// A two-stream item (generated tone, stereo, with the map a two-stream import makes) edited in
/// with `timeline.place` on A1 puts one clip per stream on A1 and the track below, and the second
/// clip plays the second stream's channels.
#[test]
fn placing_a_two_stream_item_puts_one_clip_per_stream_on_its_own_track() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "multi", "audio": 1, "video": 1})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let src = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -12.0 }, 16, 16, FrameRate::FPS_24, Tick::from_seconds_f64(2.0));
    let info = src.info().clone();
    let map = stream_channel_map(&[stream(2), stream(2)]);
    let mut p = (*s.project).clone();
    let item = p.add_item(
        "Two streams",
        Label::Violet,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(Generator::Tone { hz: 440.0, db: -12.0 }),
            info,
            interpret: Interpretation { audio_channels: map, ..Default::default() },
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    s.project = Arc::new(p);
    s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let q = s.project.sequence(seq).unwrap();
    let on: Vec<Vec<u16>> = q.audio_tracks.iter().filter_map(|t| t.items.first()).map(|c| c.source_channels.clone()).collect();
    assert_eq!(on, vec![vec![0, 1], vec![2, 3]], "one clip per stream, each on its own track");
}

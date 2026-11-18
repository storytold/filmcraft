//! Avid Log Exchange (`.ale`) clip logs (File ▸ Export ▸ Avid Log Exchange…).
//!
//! The format itself (the document, its reader and writer, the heading and `Tracks` helpers) comes
//! from [`craft_timeline::ale`] in craft-libs (craft-libs #778); this module keeps FilmCraft's
//! mapping from project items to log rows.

pub use craft_timeline::ale::{AleDoc, fps_text, parse, row_range, write};
use craft_timeline::ale::{EXTRA_COLUMNS, STANDARD_COLUMNS, clean, standard_heading, tracks_text};
use filmcraft_project::{ItemId, MediaRef, Project};
use filmcraft_time::{FrameRate, Tick, format_timecode_frames};

/// Build the log of `items` (media clips and subclips; other items are skipped). The heading's
/// frame rate is the first video item's (24 fps when there is none).
pub fn export_items(p: &Project, items: &[ItemId]) -> AleDoc {
    let media_of = |id: ItemId| -> Option<(&filmcraft_project::MediaClip, Tick, Tick)> {
        let (_, m, range) = p.resolve_media(id)?;
        let (start, dur) = range.map_or((Tick::ZERO, m.duration()), |r| (r.start, r.duration));
        Some((m, start, dur))
    };
    let rows_src: Vec<ItemId> = items.iter().copied().filter(|i| media_of(*i).is_some()).collect();
    let first_video = rows_src.iter().find_map(|i| media_of(*i).and_then(|(m, ..)| m.info.video.as_ref().map(|v| (v.frame_rate, v.height))));
    let (rate, height) = first_video.unwrap_or((FrameRate::FPS_24, 1080));
    let sr = rows_src.iter().find_map(|i| media_of(*i).and_then(|(m, ..)| m.info.audio().map(|a| a.sample_rate))).unwrap_or(48_000);
    let heading = standard_heading(rate, height, sr);
    let columns: Vec<String> = STANDARD_COLUMNS.iter().chain(EXTRA_COLUMNS.iter()).map(|s| s.to_string()).collect();
    let mut rows = Vec::new();
    for id in rows_src {
        let (Some(it), Some((m, start, dur))) = (p.item(id), media_of(id)) else { continue };
        let tracks = tracks_text(m.info.video.is_some(), m.info.audio().map_or(0, |a| a.channels));
        // media start timecode (frames at the media's own rate) + the subclip offset
        let tc0 = m.info.start_timecode.map(|f| m.frame_rate().tick_of(f)).unwrap_or(Tick::ZERO);
        let a = rate.frame_at(tc0 + start);
        let b = rate.frame_at(tc0 + start + dur);
        let path = match &m.media {
            MediaRef::File { path } => path.clone(),
            MediaRef::Generator(_) => String::new(),
        };
        let meta = |k: &str| it.metadata.iter().find(|(x, _)| x.eq_ignore_ascii_case(k)).map(|(_, v)| clean(v)).unwrap_or_default();
        let mut row = vec![
            clean(&it.name),
            tracks,
            format_timecode_frames(a, rate, false),
            format_timecode_frames(b.max(a + 1), rate, false),
            meta("Tape Name"),
            clean(&path),
        ];
        row.extend(EXTRA_COLUMNS.iter().map(|k| meta(k)));
        rows.push(row);
    }
    AleDoc { heading, columns, rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_timeline::ale::video_format;
    use filmcraft_media::{AudioStreamInfo, MediaInfo, MediaKind, VideoStreamInfo};
    use filmcraft_project::{ItemKind, Label, MediaClip};
    use filmcraft_time::TimeRange;

    #[test]
    fn non_positive_or_non_finite_fps_is_rejected() {
        for fps in ["0", "-24", "nan", "inf", "1e300", "0.0001"] {
            let doc = parse(&format!("Heading\nFPS\t{fps}\n\nColumn\nName\tStart\tEnd\n\nData\nA\t01:00:00:00\t01:00:10:00\n")).unwrap();
            assert!(doc.rate().is_none(), "FPS {fps} accepted as {:?}", doc.rate());
        }
    }

    fn media(p: &mut Project, name: &str, rate: FrameRate, seconds: i64, tc: Option<i64>, audio: u32) -> ItemId {
        let info = MediaInfo {
            name: name.into(),
            kind: MediaKind::Movie,
            duration: rate.tick_of(seconds * 24),
            video: Some(VideoStreamInfo {
                width: 1920,
                height: 1080,
                frame_rate: rate,
                par: (1, 1),
                codec: "h264".into(),
                pixel_format: "yuv420p".into(),
                color: Default::default(),
                has_alpha: false,
                bitrate: None,
                hdr: None,
                color_assumed: None,
            }),
            audio_streams: (audio > 0)
                .then(|| AudioStreamInfo { sample_rate: 48_000, channels: audio, codec: "aac".into(), bits_per_sample: None })
                .into_iter()
                .collect(),
            container: "mp4".into(),
            start_timecode: tc,
            file_size: None,
        };
        let clip = MediaClip {
            media: MediaRef::File { path: format!("/media/{name}") },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        p.add_item(name, Label::Iris, ItemKind::Media(Box::new(clip)), None)
    }

    #[test]
    fn writes_the_three_sections_and_round_trips() {
        let mut p = Project::new("ALE");
        let r = FrameRate::FPS_23_976;
        let a = media(&mut p, "Beach\tWide.mp4", r, 10, Some(86_400), 2);
        p.item_mut(a).unwrap().metadata.insert("Scene".into(), "4".into());
        p.item_mut(a).unwrap().metadata.insert("Tape Name".into(), "A001".into());
        let sub = p.add_item(
            "Beach Sub",
            Label::Iris,
            ItemKind::Subclip { parent: a, range: TimeRange::new(r.tick_of(24), r.tick_of(48)), restrict_trims: false },
            None,
        );
        let seq = p.new_sequence("Seq", Default::default(), 1, 1, None);
        let doc = export_items(&p, &[a, sub, seq]);
        let text = write(&doc);
        assert!(
            text.starts_with(
                "Heading\r\nFIELD_DELIM\tTABS\r\nVIDEO_FORMAT\t1080\r\nAUDIO_FORMAT\t48khz\r\nFPS\t23.976\r\n\r\nColumn\r\nName\tTracks\tStart\tEnd\tTape"
            ),
            "{text}"
        );
        let back = parse(&text).unwrap();
        assert_eq!(back, doc);
        assert_eq!(back.rows.len(), 2, "sequences are not logged");
        assert_eq!(back.get(0, "Name"), Some("Beach Wide.mp4"));
        assert_eq!(back.get(0, "Tracks"), Some("VA1A2"));
        assert_eq!(back.get(0, "Start"), Some("01:00:00:00"));
        assert_eq!(back.get(0, "End"), Some("01:00:10:00"));
        assert_eq!(back.get(0, "Tape"), Some("A001"));
        assert_eq!(back.get(0, "Source File"), Some("/media/Beach Wide.mp4"));
        assert_eq!(back.get(0, "Scene"), Some("4"));
        assert_eq!(back.get(1, "Start"), Some("01:00:01:00"));
        assert_eq!(back.get(1, "End"), Some("01:00:03:00"));
        let (s, e) = row_range(&back, 1).unwrap();
        assert_eq!(e - s, r.tick_of(48));
        assert_eq!(back.rate(), Some(r));
        // LF-only files read the same
        assert_eq!(parse(&text.replace("\r\n", "\n")).unwrap(), doc);
        assert!(parse("hello").is_err());
        // a UTF-8 byte order mark before `Heading` is ignored
        assert_eq!(parse(&format!("\u{feff}{text}")).unwrap(), doc);
    }

    #[test]
    fn fps_and_formats() {
        assert_eq!(fps_text(FrameRate::FPS_29_97), "29.97");
        assert_eq!(fps_text(FrameRate::FPS_25), "25");
        assert_eq!(fps_text(FrameRate::FPS_59_94), "59.94");
        assert_eq!(video_format(576), "PAL");
        assert_eq!(video_format(2160), "CUSTOM");
    }

    #[test]
    fn exports_chained_subclips_and_skips_cyclic_subclips_without_hanging() {
        let mut p = Project::new("ALE");
        let r = FrameRate::FPS_23_976;
        let a = media(&mut p, "Clip.mp4", r, 10, Some(86_400), 2);
        let sub1 =
            p.add_item("Sub 1", Label::Iris, ItemKind::Subclip { parent: a, range: TimeRange::new(r.tick_of(24), r.tick_of(48)), restrict_trims: false }, None);
        let sub2 = p.add_item(
            "Sub 2",
            Label::Iris,
            ItemKind::Subclip { parent: sub1, range: TimeRange::new(r.tick_of(10), r.tick_of(20)), restrict_trims: false },
            None,
        );
        // Cyclic subclip
        let cycle = p.add_item("Cycle", Label::Iris, ItemKind::Subclip { parent: ItemId(999), range: TimeRange::default(), restrict_trims: false }, None);
        p.item_mut(cycle).unwrap().kind = ItemKind::Subclip { parent: cycle, range: TimeRange::default(), restrict_trims: false };

        let doc = export_items(&p, &[sub2, cycle]);
        // Chained subclip succeeds, cyclic subclip is cleanly skipped
        assert_eq!(doc.rows.len(), 1);
        assert_eq!(doc.get(0, "Name"), Some("Sub 2"));
        assert_eq!(doc.get(0, "Source File"), Some("/media/Clip.mp4"));
    }
}

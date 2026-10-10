//! Avid Log Exchange (`.ale`): a tab-delimited clip log (File ▸ Export ▸ Avid Log Exchange…).
//!
//! Written from Avid's published description of the format: three sections introduced by a line
//! holding only `Heading`, `Column` or `Data`, separated by blank lines.
//!
//! ```text
//! Heading
//! FIELD_DELIM  TABS
//! VIDEO_FORMAT  1080
//! AUDIO_FORMAT  48khz
//! FPS  23.976
//!
//! Column
//! (fields are separated by tab characters)
//! Name  Tracks  Start  End  Tape  Source File  Description  …
//!
//! Data
//! Beach  VA1A2  01:00:00:00  01:00:10:00  …
//! ```
//!
//! `Tracks` lists the clip's tracks (`V`, `A1`…`A16`); `Start` and `End` are SMPTE timecode at the
//! heading's `FPS`, `End` exclusive. Values never contain tabs or line breaks (they are replaced by
//! spaces). Lines end with CR LF (Avid's own files); the reader accepts LF and CR too.

use filmcraft_project::{ItemId, ItemKind, MediaRef, Project};
use filmcraft_time::{FrameRate, Tick, format_timecode_frames, parse_timecode};

/// A parsed or to-be-written ALE file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AleDoc {
    /// Heading fields in order (`FIELD_DELIM`, `VIDEO_FORMAT`, `AUDIO_FORMAT`, `FPS`, …).
    pub heading: Vec<(String, String)>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl AleDoc {
    pub fn heading(&self, key: &str) -> Option<&str> {
        self.heading.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
    /// The value of `column` in row `row`.
    pub fn get(&self, row: usize, column: &str) -> Option<&str> {
        let c = self.columns.iter().position(|x| x.eq_ignore_ascii_case(column))?;
        self.rows.get(row)?.get(c).map(String::as_str)
    }
    /// The heading's frame rate (`FPS`).
    pub fn rate(&self) -> Option<FrameRate> {
        self.heading("FPS").and_then(|f| f.trim().parse::<f64>().ok()).map(FrameRate::from_f64)
    }
}

/// Metadata columns written after the standard ones (from the items' metadata fields).
const EXTRA: [&str; 6] = ["Description", "Scene", "Take", "Shot", "Log Note", "Comment"];

fn clean(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

/// `FPS` heading text for a rate.
pub fn fps_text(r: FrameRate) -> String {
    let f = r.as_f64();
    if (f - f.round()).abs() < 1e-6 { format!("{}", f.round() as i64) } else { format!("{:.3}", f).trim_end_matches('0').to_string() }
}

/// `VIDEO_FORMAT` heading text for a frame height.
fn video_format(height: u32) -> &'static str {
    match height {
        1080 => "1080",
        720 => "720",
        480 | 486 => "NTSC",
        576 => "PAL",
        _ => "CUSTOM",
    }
}

/// Build the log of `items` (media clips and subclips; other items are skipped). The heading's
/// frame rate is the first video item's (24 fps when there is none).
pub fn export_items(p: &Project, items: &[ItemId]) -> AleDoc {
    let media_of = |id: ItemId| -> Option<(&filmcraft_project::MediaClip, Tick, Tick)> {
        let it = p.item(id)?;
        match &it.kind {
            ItemKind::Media(m) => Some((m, Tick::ZERO, m.duration())),
            ItemKind::Subclip { parent, range, .. } => p.item(*parent)?.as_media().map(|m| (m, range.start, range.duration)),
            _ => None,
        }
    };
    let rows_src: Vec<ItemId> = items.iter().copied().filter(|i| media_of(*i).is_some()).collect();
    let first_video = rows_src.iter().find_map(|i| media_of(*i).and_then(|(m, ..)| m.info.video.as_ref().map(|v| (v.frame_rate, v.height))));
    let (rate, height) = first_video.unwrap_or((FrameRate::FPS_24, 1080));
    let sr = rows_src.iter().find_map(|i| media_of(*i).and_then(|(m, ..)| m.info.audio().map(|a| a.sample_rate))).unwrap_or(48_000);
    let heading = vec![
        ("FIELD_DELIM".to_string(), "TABS".to_string()),
        ("VIDEO_FORMAT".to_string(), video_format(height).to_string()),
        ("AUDIO_FORMAT".to_string(), if sr == 44_100 { "44khz" } else { "48khz" }.to_string()),
        ("FPS".to_string(), fps_text(rate)),
    ];
    let mut columns: Vec<String> = ["Name", "Tracks", "Start", "End", "Tape", "Source File"].iter().map(|s| s.to_string()).collect();
    columns.extend(EXTRA.iter().map(|s| s.to_string()));
    let mut rows = Vec::new();
    for id in rows_src {
        let (Some(it), Some((m, start, dur))) = (p.item(id), media_of(id)) else { continue };
        let mut tracks = String::new();
        if m.info.video.is_some() {
            tracks.push('V');
        }
        for c in 1..=m.info.audio().map_or(0, |a| a.channels.min(16)) {
            tracks.push_str(&format!("A{c}"));
        }
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
        row.extend(EXTRA.iter().map(|k| meta(k)));
        rows.push(row);
    }
    AleDoc { heading, columns, rows }
}

/// Serialise (CR LF line ends).
pub fn write(doc: &AleDoc) -> String {
    let mut out = String::from("Heading\r\n");
    for (k, v) in &doc.heading {
        out.push_str(&format!("{}\t{}\r\n", clean(k), clean(v)));
    }
    out.push_str("\r\nColumn\r\n");
    out.push_str(&doc.columns.iter().map(|c| clean(c)).collect::<Vec<_>>().join("\t"));
    out.push_str("\r\n\r\nData\r\n");
    for r in &doc.rows {
        out.push_str(&r.iter().map(|c| clean(c)).collect::<Vec<_>>().join("\t"));
        out.push_str("\r\n");
    }
    out
}

/// Read an ALE file.
pub fn parse(text: &str) -> Result<AleDoc, String> {
    #[derive(PartialEq)]
    enum Sec {
        None,
        Heading,
        Column,
        Data,
    }
    let mut doc = AleDoc::default();
    let mut sec = Sec::None;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    for line in text.lines() {
        match line.trim() {
            "Heading" => {
                sec = Sec::Heading;
                continue;
            }
            "Column" => {
                sec = Sec::Column;
                continue;
            }
            "Data" => {
                sec = Sec::Data;
                continue;
            }
            "" => continue,
            _ => {}
        }
        match sec {
            Sec::None => return Err("not an ALE file (no Heading section)".into()),
            Sec::Heading => {
                let mut it = line.splitn(2, '\t');
                let k = it.next().unwrap_or_default().trim().to_string();
                doc.heading.push((k, it.next().unwrap_or_default().trim().to_string()));
            }
            Sec::Column => doc.columns.extend(line.split('\t').map(|c| c.trim().to_string())),
            Sec::Data => doc.rows.push(line.split('\t').map(str::to_string).collect()),
        }
    }
    if doc.columns.is_empty() {
        return Err("the ALE file has no Column section".into());
    }
    Ok(doc)
}

/// Start and end (exclusive) of a data row, in ticks at the heading's rate.
pub fn row_range(doc: &AleDoc, row: usize) -> Option<(Tick, Tick)> {
    let rate = doc.rate()?;
    let a = parse_timecode(doc.get(row, "Start")?, rate, false, 0).ok()?;
    let b = parse_timecode(doc.get(row, "End")?, rate, false, 0).ok()?;
    Some((rate.tick_of(a), rate.tick_of(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_media::{AudioStreamInfo, MediaInfo, MediaKind, VideoStreamInfo};
    use filmcraft_project::{Label, MediaClip};
    use filmcraft_time::TimeRange;

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
        p.add_item(name, Label::Iris, ItemKind::Media(clip), None)
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
    }

    #[test]
    fn fps_and_formats() {
        assert_eq!(fps_text(FrameRate::FPS_29_97), "29.97");
        assert_eq!(fps_text(FrameRate::FPS_25), "25");
        assert_eq!(fps_text(FrameRate::FPS_59_94), "59.94");
        assert_eq!(video_format(576), "PAL");
        assert_eq!(video_format(2160), "CUSTOM");
    }
}

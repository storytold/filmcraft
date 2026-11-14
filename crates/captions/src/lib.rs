//! FilmCraft captions.
//!
//! - **Files:** SubRip, WebVTT, Scenarist SCC, MacCaption MCC, EBU STL and TTML / DFXP come from
//!   [`craft_captions`] in craft-libs, re-exported here (`filmcraft_captions::srt`, [`parse`],
//!   [`write`], [`Document`] and the rest). Format fixes go there.
//! - **Model:** [`track_from_document`] / [`document_from_track`] convert to and from the project's
//!   [`CaptionTrack`]; [`track_format`] names the track format a file maps to.
//! - **Writing:** [`write_options`] fills in FilmCraft's name as the creating program (MCC, EBU STL).
//! - **Burn-in:** [`burn`] lays a caption out with the track style and draws it with the
//!   `filmcraft-text` engine (Inter SemiBold, shaped and kerned).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod burn;

pub use craft_captions::*;

use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, TrackId};

/// The creating program FilmCraft's MCC and EBU STL files name.
pub const PROGRAM: &str = "FilmCraft";

/// Write options with FilmCraft as the creating program.
pub fn write_options(drop_frame: bool, rate: Option<FrameRate>) -> WriteOptions {
    WriteOptions { drop_frame, rate, program: PROGRAM, ..WriteOptions::default() }
}

/// The caption track format a file of this kind maps to.
pub fn track_format(format: Format) -> CaptionFormat {
    match format {
        Format::Scc => CaptionFormat::Cea608,
        Format::Mcc => CaptionFormat::Cea708,
        Format::Stl => CaptionFormat::Teletext,
        _ => CaptionFormat::Subtitle,
    }
}

/// Build a caption track from a document, in the document's language when it has one (else
/// the default, `en`). Ids are allocated with `alloc`.
pub fn track_from_document(doc: &Document, id: TrackId, name: &str, format: CaptionFormat, alloc: &mut dyn FnMut() -> u64) -> CaptionTrack {
    let mut t = CaptionTrack::new(id, name.to_string(), format);
    if let Some(language) = &doc.language {
        t.language = language.clone();
    }
    t.vtt_blocks = doc.blocks.clone();
    t.captions = doc
        .cues
        .iter()
        .filter(|c| c.end > c.start)
        .map(|c| Caption {
            id: ClipId(alloc()),
            start: c.start,
            duration: c.end - c.start,
            text: c.text.clone(),
            speaker: c.speaker.clone(),
            cue_id: c.id.clone(),
            settings: c.settings.clone(),
        })
        .collect();
    t.sort();
    t
}

/// The document for a caption track (for export). `offset` is subtracted from every time (export
/// of a range starting later than zero).
pub fn document_from_track(t: &CaptionTrack, offset: Tick) -> Document {
    Document {
        cues: t
            .captions
            .iter()
            .filter(|c| c.end() > offset)
            .map(|c| Cue {
                start: (c.start - offset).max(Tick::ZERO),
                end: c.end() - offset,
                text: c.text.clone(),
                speaker: c.speaker.clone(),
                id: c.cue_id.clone(),
                settings: c.settings.clone(),
            })
            .collect(),
        blocks: t.vtt_blocks.clone(),
        language: Some(t.language.clone()).filter(|l| !l.is_empty()),
        ..Document::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filmcraft_names_itself_in_mcc_and_stl() {
        let doc = Document {
            cues: vec![Cue { start: Tick(0), end: Tick(filmcraft_time::TICKS_PER_SECOND), text: "Hi".into(), ..Default::default() }],
            ..Default::default()
        };
        let mcc = String::from_utf8(write(&doc, Format::Mcc, write_options(true, None))).unwrap();
        assert!(mcc.contains("Creation Program=FilmCraft"), "{mcc}");
        let stl = write(&doc, Format::Stl, write_options(false, None));
        assert!(stl.windows(PROGRAM.len()).any(|w| w == PROGRAM.as_bytes()));
    }

    #[test]
    fn track_formats() {
        assert_eq!(track_format(Format::Scc), CaptionFormat::Cea608);
        assert_eq!(track_format(Format::Mcc), CaptionFormat::Cea708);
        assert_eq!(track_format(Format::Stl), CaptionFormat::Teletext);
        assert_eq!(track_format(Format::Srt), CaptionFormat::Subtitle);
    }
}

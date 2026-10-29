//! OMF Interchange 2.0 export (and import of the same structures) for audio post.
//!
//! Written from the public *OMF Interchange Specification Version 2.0* (Avid Technology, 1995/97)
//! and Apple's *Bento Specification* revision 1.0d5 for the container ([`bento`]).
//!
//! The file holds a composition mob (`CMOB`) with a timecode slot and one sound slot per audio
//! track (one per channel when broken out to mono): sequences (`SEQU`) of fillers (`FILL`), source
//! clips (`SCLP`) and transitions (`TRAN` with a `SimpleMonoAudioDissolve` effect); clip volume is a
//! `MonoAudioGain` effect (`EFFE`) whose slot −1 is the clip and slot 1 the amplitude
//! (`CVAL` constant or `VVAL` keyframes). Every media item has a master mob (`MMOB`), a file source
//! mob (`SMOB` with a `WAVD` / `AIFD` descriptor: embedded essence in a `WAVE` media data object,
//! or a locator to a separate file) and a tape source mob carrying its timecode. Times are in
//! samples (sound slots use the sample rate as edit rate) so edits are sample-exact.
//!
//! A nested sequence is not written as a composition: its sound, mixed by the caller for the clip
//! ([`crate::essence::NestNeeds::Render`]), is media like any other, in a clip named after the
//! sequence. Premiere Pro's OMF export does the same.

pub(crate) mod bento;
mod read;
mod write;

use filmcraft_project::{ItemId, Project};
use serde::{Deserialize, Serialize};

use crate::comp::{self, ExtractedMedia};
use crate::essence::MediaOptions;
use crate::{ImportOptions, Imported, Report, Result};

/// OMF export options (Premiere's OMF export dialog: the caller renders / consolidates the audio
/// and passes it in [`MediaOptions::essence`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OmfOptions {
    /// Composition name (default: the sequence name).
    pub name: Option<String>,
    pub media: MediaOptions,
}

/// Export the audio of `sequence` as an OMF 2.0 file.
pub fn export(project: &Project, sequence: ItemId, opts: &OmfOptions) -> Result<(Vec<u8>, Report)> {
    let mut report = Report::default();
    let name = opts.name.clone().or_else(|| project.item(sequence).map(|i| i.name.clone())).unwrap_or_else(|| "Sequence".into());
    let mut media = opts.media.clone();
    media.audio_only = true;
    media.mixdown_video = None;
    let doc = comp::from_project(project, sequence, &name, &media, comp::Nests::Rendered, &mut report)?;
    if !doc.compositions[0].markers.is_empty() {
        report.info("sequence markers are not written to OMF");
    }
    let bytes = write::write(&doc, &mut report)?;
    Ok((bytes, report))
}

/// Import an OMF 2.0 file (as written by [`export`] and by applications using the same
/// structures). Embedded audio is returned as WAV files.
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Vec<ExtractedMedia>, Report)> {
    let mut report = Report::default();
    let doc = read::read(bytes, &mut report)?;
    let (imported, extracted) = comp::to_project(doc, opts, &mut report)?;
    Ok((imported, extracted, report))
}

/// Whether `bytes` end with a Bento container label (OMF files do).
pub fn sniff(bytes: &[u8]) -> bool {
    bento::sniff(bytes)
}

#[cfg(test)]
mod tests;

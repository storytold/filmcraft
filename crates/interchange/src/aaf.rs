//! AAF (Advanced Authoring Format) import and export, following the AAF Edit Protocol.
//!
//! Written from the public AMWA specifications: *AAF Object Specification v1.1*, *AAF Low-Level
//! Container Specification v1.0.1* (structured storage mapping, see [`store`]) and *AAF Edit
//! Protocol v1.0*; the container is Microsoft's Compound File Binary format ([`filmcraft_cfb`]).
//!
//! The exported file holds one top-level **CompositionMob** (Edit Protocol usage code
//! "top level") with
//!
//! - a timecode slot (sequence start timecode, rounded rate, drop frame),
//! - one timeline slot per video track (picture) and per audio track (sound; one per channel when
//!   broken out to mono), each a **Sequence** of **Filler**s, **SourceClip**s and **Transition**s
//!   (Cross Dissolve → `VideoDissolve_2`, Dip to Black → `VideoFadeToBlack`, audio crossfades →
//!   `MonoAudioDissolve`; the FilmCraft effect id is kept in a tagged value),
//! - clip volume as a `MonoAudioGain` **OperationGroup** around the source clip with an
//!   `Amplitude` **ConstantValue** or **VaryingValue** (keyframes as control points),
//! - sequence markers as **CommentMarker**s in an event slot;
//!
//! a **CompositionMob** without a usage code for every nested sequence, built the same way (a
//! clip of the nested sequence is a **SourceClip** that points at that mob, at its first slot of
//! the clip's kind, with its start in that slot's edit units; Premiere Pro writes nested sequences
//! this way);
//!
//! and for every media item a **MasterMob**, a file **SourceMob** per essence (a CDCI or PCM
//! descriptor with a network locator to the file, or `EssenceData` holding embedded PCM) and a
//! tape **SourceMob** carrying the media's start timecode. Clip and media markers become comment
//! markers on the master mob.
//!
//! Import reads the same structures (and the common variations: sequences inside sequences,
//! selectors, operation groups around clips, legacy data definitions, essence groups, and Premiere
//! Pro's one video slot holding a nested scope with a segment per video track) into FilmCraft
//! sequences (a composition that others use becomes a nested sequence), media items (linked by path; embedded audio is returned as WAV files for the
//! caller to write) and markers.

mod ids;
mod read;
pub(crate) mod store;
mod write;

use filmcraft_project::{ItemId, Project};
use serde::{Deserialize, Serialize};

use crate::comp::{self, ExtractedMedia};
use crate::essence::MediaOptions;
use crate::{ImportOptions, Imported, Report, Result};

/// AAF export options (Premiere's AAF export dialog maps onto [`MediaOptions`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AafOptions {
    /// Composition name (default: the sequence name).
    pub name: Option<String>,
    pub media: MediaOptions,
    /// Write 512-byte sectors (compound file version 3) instead of 4096-byte sectors.
    pub small_sectors: bool,
}

/// Export `sequence` as an AAF file.
pub fn export(project: &Project, sequence: ItemId, opts: &AafOptions) -> Result<(Vec<u8>, Report)> {
    let mut report = Report::default();
    let name = opts.name.clone().or_else(|| project.item(sequence).map(|i| i.name.clone())).unwrap_or_else(|| "Sequence".into());
    let doc = comp::from_project(project, sequence, &name, &opts.media, comp::Nests::Compositions, &mut report)?;
    let version = if opts.small_sectors { filmcraft_cfb::Version::V3 } else { filmcraft_cfb::Version::V4 };
    let bytes = write::write(&doc, version, &mut report)?;
    Ok((bytes, report))
}

/// Import an AAF file: its top-level compositions become sequences. Embedded audio is returned
/// as WAV files whose paths are the paths of the imported media items.
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Vec<ExtractedMedia>, Report)> {
    let mut report = Report::default();
    let doc = read::read(bytes, &mut report)?;
    let (imported, extracted) = comp::to_project(doc, opts, &mut report)?;
    Ok((imported, extracted, report))
}

/// Whether `bytes` look like an AAF file (a compound file whose root has the AAF root class or a
/// `Header-2` storage).
pub fn sniff(bytes: &[u8]) -> bool {
    if !filmcraft_cfb::sniff(bytes) {
        return false;
    }
    match filmcraft_cfb::CompoundFile::open(bytes) {
        Ok(cf) => cf.root().clsid == ids::ROOT || cf.find("Header-2").is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod hostile_tests;
#[cfg(test)]
mod tests;

//! Clean-room Matroska (MKV) and WebM demuxer, written from RFC 8794 (EBML), RFC 9559 (Matroska)
//! and the WebM container guidelines.
//!
//! - [`open`] / [`open_with`] parse the EBML header and the first Segment (Info, Tracks, SeekHead,
//!   Cues, Chapters, Tags, Attachments) and, by default, scan every Cluster's block headers to build
//!   per-track [`Sample`] tables — the same shape as `filmcraft-isobmff`, so callers can treat both
//!   containers alike. Frame data is read on demand with [`MkvFile::read_sample`].
//! - [`Demuxer`] iterates [`Packet`]s in file order (from any [`ByteSource`], `Read + Seek`, or a
//!   byte slice) and seeks to the keyframe preceding a time, via the index, Cues, or a cluster scan.
//!
//! Layer L0: no dependencies beyond `std`, no `unsafe`, builds for `wasm32-unknown-unknown`.

mod codec;
mod demux;
pub mod ebml;
mod error;
mod ids;
mod meta;
mod mux;
mod source;
#[cfg(test)]
mod synthetic_tests;
mod track;

pub use codec::Codec;
pub use demux::{Demuxer, Keyframe, MkvFile, OpenOptions, Packet, SeekPoint, open, open_with};
pub use error::{Error, Result};
pub use meta::{Attachment, Chapter, ChapterDisplay, ClusterInfo, CuePoint, CuePosition, Edition, SeekEntry, SegmentInfo, SimpleTag, Tag, TagTargets};
pub use mux::{LacingMode, MkvWriter, MuxOptions, TrackSpec};
pub use source::{ByteSource, ReadSeekSource};
pub use track::{AudioInfo, Colour, ContentEncoding, MasteringMetadata, Projection, Sample, Track, TrackKind, VideoInfo};

/// Element IDs used by the demuxer (marker bits included), for callers inspecting raw elements.
pub mod element_ids {
    pub use crate::ids::*;
}

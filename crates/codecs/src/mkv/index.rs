//! Progressive indexing of a Matroska/WebM file whose bytes are still arriving (the web build reads
//! a file in chunks, see [`filmcraft_media::ByteReader::ready_prefix`]).
//!
//! The source opens with the sample tables of the front of the file only (Info, Tracks and the
//! Clusters that are loaded), which is enough to show the first picture. A request past what is
//! indexed asks the reader for more: it indexes the Clusters that have arrived since
//! ([`MkvFile::extend_index`], no rescan), derives the tables again and carries on. When nothing new
//! has arrived the request fails like any read of unloaded data (`WouldBlock`, with
//! [`filmcraft_media::pending`] set): the caller shows its placeholder and asks again later.
//!
//! The index only ever grows: sample numbers keep their meaning, so decoded pictures cached by sample
//! stay valid. Seeking far ahead waits for the bytes in between (the tables are built in file order;
//! Cues are not used for random access).

use std::sync::{Arc, PoisonError};

use filmcraft_matroska::{MkvFile, OpenOptions, TrackKind};
use filmcraft_media::MediaError;

use super::{MkvSource, audio_starts};

/// Audio is served from the index once some track is indexed this far past the end of the window.
pub(super) const AUDIO_MARGIN_NS: i128 = 100_000_000;

/// The sample tables of one moment of the index, with what is derived from them.
pub(super) struct Tables {
    pub(super) file: MkvFile,
    /// Packet start positions in source sample frames, per audio stream.
    pub(super) starts: Vec<Vec<i64>>,
    /// End of the indexed part of the video track, in its timestamp units (`None`: no samples).
    pub(super) video_end: Option<i64>,
    /// The latest indexed timestamp of any track, in nanoseconds.
    pub(super) reach_ns: i128,
}

impl Tables {
    pub(super) fn new(file: MkvFile, starts: Vec<Vec<i64>>, vtrack: Option<usize>) -> Self {
        let video_end = vtrack
            .and_then(|i| file.tracks.get(i))
            .and_then(|t| t.samples.iter().map(|s| s.pts.saturating_add(i64::try_from(s.duration).unwrap_or(i64::MAX))).max());
        let reach_ns = file.tracks.iter().filter_map(|t| t.samples.iter().map(|s| s.pts).max().map(|p| i128::from(t.ticks_to_ns(p)))).max().unwrap_or(0);
        Self { file, starts, video_end, reach_ns }
    }

    /// Every Cluster is indexed.
    pub(super) fn complete(&self) -> bool {
        self.file.index_end.is_none()
    }
}

/// Open the index of `bytes`: all of it, or (for a source whose bytes are still arriving) the front
/// of it, when that is enough to serve the file: the duration comes from Info and every video and
/// audio track has samples there. Otherwise the whole index is built, as for any file.
pub(super) fn open_index(bytes: &crate::Src) -> crate::Result<MkvFile> {
    let container = |e: filmcraft_matroska::Error| crate::CodecError::Container(e.to_string());
    let len = bytes.0.len();
    let ready = bytes.0.ready_prefix().min(len);
    let opts = OpenOptions { index_limit: (ready < len).then_some(ready), ..OpenOptions::default() };
    let mut file = filmcraft_matroska::open_with(bytes, &opts).map_err(container)?;
    let served_from_the_front =
        file.info.duration.is_some() && file.tracks.iter().filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio)).all(|t| !t.samples.is_empty());
    if file.index_end.is_some() && !served_from_the_front {
        file.extend_index(bytes, &OpenOptions::default()).map_err(container)?;
    }
    Ok(file)
}

impl MkvSource {
    /// The current tables.
    pub(super) fn tables(&self) -> Arc<Tables> {
        self.tables.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// `tables`, or newer ones, once `covered` holds (or the index is complete). Fails with
    /// `WouldBlock` semantics (see the module docs) when more of the file has to arrive first.
    pub(super) fn ensure(&self, mut tables: Arc<Tables>, covered: impl Fn(&Tables) -> bool) -> Result<Arc<Tables>, MediaError> {
        while !tables.complete() && !covered(&tables) {
            tables = self.grow_index(&tables)?;
        }
        Ok(tables)
    }

    /// Index the Clusters that have arrived since `seen`; a newer index than `seen` (another thread
    /// was first) is returned as it is. Errors with the reader's `WouldBlock` when nothing new could
    /// be indexed, after asking it for the next bytes.
    fn grow_index(&self, seen: &Arc<Tables>) -> Result<Arc<Tables>, MediaError> {
        let _one_at_a_time = self.grow.lock().unwrap_or_else(PoisonError::into_inner);
        let cur = self.tables();
        let Some(end) = cur.file.index_end.filter(|_| Arc::ptr_eq(&cur, seen)) else { return Ok(cur) };
        let len = self.bytes.0.len();
        let ready = self.bytes.0.ready_prefix().min(len);
        let limit = (ready < len).then_some(ready);
        if limit.is_some_and(|l| l <= end) {
            return self.wait_for(end);
        }
        let mut file = cur.file.clone();
        let before: Vec<usize> = file.tracks.iter().map(|t| t.samples.len()).collect();
        file.extend_index(&self.bytes, &OpenOptions { index_limit: limit, ..OpenOptions::default() }).map_err(|e| MediaError::Io(e.to_string()))?;
        if file.index_end == cur.file.index_end {
            // the next Cluster does not fit in what is loaded
            return self.wait_for(ready);
        }
        for (t, &from) in file.tracks.iter_mut().zip(&before) {
            if self.origin > 0 {
                t.samples.iter_mut().skip(from).for_each(|s| s.pts = s.pts.saturating_sub(self.origin));
            }
        }
        // the alpha table first: a reader of the new tables finds it complete
        if let (Some(layer), Some(vt)) = (&self.alpha, self.vtrack)
            && let (Some(t), Some(&from)) = (file.tracks.get(vt), before.get(vt))
        {
            layer.extend(t.samples.iter().skip(from).map(|s| (s.pts, s.addition)));
        }
        let starts =
            self.audios.iter().zip(&self.info.audio_streams).map(|(a, info)| audio_starts(&file, &self.bytes, a.track, i64::from(info.sample_rate))).collect();
        let grown = Arc::new(Tables::new(file, starts, self.vtrack));
        *self.tables.write().unwrap_or_else(PoisonError::into_inner) = grown.clone();
        Ok(grown)
    }

    /// Ask the reader for the bytes from `at` on (a read of data that is not loaded makes it fetch
    /// them) and report that they are not here yet.
    fn wait_for(&self, at: u64) -> Result<Arc<Tables>, MediaError> {
        let mut probe = [0u8; 1];
        match self.bytes.0.read_at(at, &mut probe) {
            // served after all (it arrived meanwhile): ask again, the caller retries
            Ok(()) => Err(MediaError::Io(filmcraft_media::pending::would_block().to_string())),
            // a reader that is still fetching marked the read as deferred (`WouldBlock`)
            Err(e) => Err(MediaError::Io(e.to_string())),
        }
    }
}

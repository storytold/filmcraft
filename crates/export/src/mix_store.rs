//! A temporary on-disk copy of the mixed (and channel-folded) export range, so the loudness
//! passes mix the sequence once and every later pass reads it back.
//!
//! The file holds interleaved little-endian `f32` frames, so what is read back is bit-identical to
//! what was written. It is removed when the store is dropped.

use std::fs::File;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Bytes per stored sample.
const SAMPLE_BYTES: usize = 4;

/// Frames decoded per file read.
const READ_BLOCK_FRAMES: usize = 1 << 16;

pub(crate) struct MixStore {
    file: File,
    path: PathBuf,
    /// First stored frame (sample position).
    start: i64,
    /// Frames written.
    len: i64,
    /// Folded output channel count (what `AudioOut::mix` returns).
    channels: usize,
}

fn invalid(msg: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, msg)
}

impl MixStore {
    /// Create an empty store for `channels` channels starting at sample `start`, in `dir` (the
    /// system temporary directory when None). None when the file cannot be created, and always
    /// None on wasm, which has no temporary directory.
    pub fn create(start: i64, channels: usize, dir: Option<&Path>) -> Option<Self> {
        if channels == 0 {
            return None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let dir = dir.map_or_else(std::env::temp_dir, Path::to_path_buf);
            let name = format!("filmcraft-loudness-{}-{}.f32", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
            let path = dir.join(name);
            let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).ok()?;
            Some(MixStore { file, path, start, len: 0, channels })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (start, dir);
            None
        }
    }

    /// Append planar frames (`channels` equal-length planes) after the ones already stored.
    pub fn append(&mut self, planar: &[Vec<f32>]) -> std::io::Result<()> {
        if planar.len() != self.channels {
            return Err(invalid("channel count mismatch"));
        }
        let n = planar.first().map_or(0, Vec::len);
        if planar.iter().any(|c| c.len() != n) {
            return Err(invalid("ragged planes"));
        }
        let bytes = n.checked_mul(self.channels).and_then(|v| v.checked_mul(SAMPLE_BYTES)).ok_or_else(|| invalid("chunk too large"))?;
        let mut buf = Vec::with_capacity(bytes);
        for i in 0..n {
            for c in planar {
                buf.extend_from_slice(&c.get(i).copied().unwrap_or(0.0).to_le_bytes());
            }
        }
        let frames = i64::try_from(n).map_err(|_| invalid("chunk too large"))?;
        let len = self.len.checked_add(frames).ok_or_else(|| invalid("store too long"))?;
        (&self.file).write_all(&buf)?;
        self.len = len;
        Ok(())
    }

    /// Make every appended frame readable.
    pub fn finish(&mut self) -> std::io::Result<()> {
        (&self.file).flush()
    }

    /// `n` frames from `pos`, planar. Frames past the stored end are 0.0; a `pos` before the
    /// stored start is an error.
    pub fn read(&self, pos: i64, n: usize) -> std::io::Result<Vec<Vec<f32>>> {
        let first = pos.checked_sub(self.start).filter(|d| *d >= 0).ok_or_else(|| invalid("read before the stored range"))?;
        let stored = self.len.saturating_sub(first).max(0);
        let avail = usize::try_from(stored).unwrap_or(usize::MAX).min(n);
        let mut out = vec![vec![0.0f32; n]; self.channels];
        if avail == 0 {
            return Ok(out);
        }
        let frame_bytes = self.channels.checked_mul(SAMPLE_BYTES).ok_or_else(|| invalid("frame too large"))?;
        let offset = u64::try_from(first).ok().and_then(|f| f.checked_mul(frame_bytes as u64)).ok_or_else(|| invalid("offset overflow"))?;
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))?;
        // in bounded blocks, so a long read does not hold the range twice
        let mut bytes = vec![0u8; READ_BLOCK_FRAMES.min(avail).checked_mul(frame_bytes).ok_or_else(|| invalid("read too large"))?];
        let mut done = 0;
        while done < avail {
            let frames = READ_BLOCK_FRAMES.min(avail - done);
            let block = bytes.get_mut(..frames * frame_bytes).ok_or_else(|| invalid("read block"))?;
            file.read_exact(block)?;
            for (i, frame) in block.chunks_exact(frame_bytes).enumerate() {
                let (samples, _) = frame.as_chunks::<SAMPLE_BYTES>();
                for (plane, raw) in out.iter_mut().zip(samples) {
                    if let Some(slot) = plane.get_mut(done + i) {
                        *slot = f32::from_le_bytes(*raw);
                    }
                }
            }
            done += frames;
        }
        Ok(out)
    }
}

impl Drop for MixStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

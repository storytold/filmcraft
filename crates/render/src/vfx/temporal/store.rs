//! Finished Warp Stabilizer analyses on disk, so a clip is analysed once, not once per session.
//!
//! One file per analysis, `<hash>.stab` in the folder [`set_dir`] names (the engine points it into
//! the media cache). A file holds the [`FxEnv::analysis_identity`](crate::vfx::FxEnv) bytes it was
//! made for, followed by the path:
//!
//! ```text
//! magic "FCSTAB" 0 1 | identity length u32 | identity | width u32 | height u32 | fps f64
//! | steps u32 | steps × (a b c d e f) f64        (little-endian)
//! ```
//!
//! The file is input like any other: a short, long, foreign or corrupt one is ignored (the clip
//! is analysed again and the file replaced), and the identity is compared in full, so two
//! identities with the same file name never share a path. Files are written whole under a
//! temporary name and renamed, so a crash leaves no half-written analysis behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use filmcraft_geom::Affine;

use super::{MAX_ANALYSIS_FRAMES, StabilizerPath};

/// Format and tracker version: bump it when the file layout or the analysis changes, so paths
/// made by an older tracker are made again.
const MAGIC: &[u8; 8] = b"FCSTAB\x00\x01";
/// Longest identity a file may hold.
const MAX_IDENTITY: usize = 1024;
/// Largest frame side a file may claim.
const MAX_SIDE: u32 = 1 << 16;

fn dir_slot() -> &'static Mutex<Option<PathBuf>> {
    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    &DIR
}

/// Keep finished analyses in `dir` (created when the first one is written); `None` keeps them in
/// memory only.
pub fn set_dir(dir: Option<PathBuf>) {
    *dir_slot().lock().unwrap_or_else(PoisonError::into_inner) = dir;
}

fn dir() -> Option<PathBuf> {
    dir_slot().lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// 64-bit FNV-1a: stable across builds and platforms (the file name of an identity).
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn file(dir: &Path, identity: &[u8]) -> PathBuf {
    dir.join(format!("{:016x}.stab", fnv(identity)))
}

/// The analysis kept for `identity`, if there is a valid one.
pub(crate) fn load(identity: &[u8]) -> Option<StabilizerPath> {
    let path = file(&dir()?, identity);
    let max = MAGIC.len() + 4 + MAX_IDENTITY + 4 + 4 + 8 + 4 + MAX_ANALYSIS_FRAMES * 48;
    if std::fs::metadata(&path).ok()?.len() > max as u64 {
        return None;
    }
    decode(&std::fs::read(&path).ok()?, identity)
}

/// Keep `path` as the analysis of `identity`. Failing to write it (read-only or full disk) only
/// means the clip is analysed again next time.
pub(crate) fn save(identity: &[u8], path: &StabilizerPath) {
    let Some(dir) = dir() else { return };
    let Some(bytes) = encode(identity, path) else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    static N: AtomicU64 = AtomicU64::new(0);
    let to = file(&dir, identity);
    let part = to.with_extension(format!("stab.{}.{}.part", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    if std::fs::write(&part, bytes).is_ok() && std::fs::rename(&part, &to).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&part);
}

pub(crate) fn encode(identity: &[u8], path: &StabilizerPath) -> Option<Vec<u8>> {
    if identity.len() > MAX_IDENTITY || path.steps.len() > MAX_ANALYSIS_FRAMES {
        return None;
    }
    let mut b = Vec::with_capacity(MAGIC.len() + identity.len() + 24 + path.steps.len() * 48);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&(identity.len() as u32).to_le_bytes());
    b.extend_from_slice(identity);
    b.extend_from_slice(&u32::try_from(path.w).ok()?.to_le_bytes());
    b.extend_from_slice(&u32::try_from(path.h).ok()?.to_le_bytes());
    b.extend_from_slice(&path.fps.to_le_bytes());
    b.extend_from_slice(&(path.steps.len() as u32).to_le_bytes());
    for m in &path.steps {
        for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    Some(b)
}

/// Reads little-endian values from the front of a byte slice.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?)).filter(|v| v.is_finite())
    }
}

pub(crate) fn decode(bytes: &[u8], identity: &[u8]) -> Option<StabilizerPath> {
    let mut r = Reader(bytes);
    if r.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let len = r.u32()? as usize;
    if len > MAX_IDENTITY || r.take(len)? != identity {
        return None;
    }
    let (w, h) = (r.u32()?, r.u32()?);
    if !(1..=MAX_SIDE).contains(&w) || !(1..=MAX_SIDE).contains(&h) {
        return None;
    }
    let fps = r.f64().filter(|f| *f > 0.0)?;
    let n = r.u32()? as usize;
    if n > MAX_ANALYSIS_FRAMES || r.0.len() != n.checked_mul(48)? {
        return None;
    }
    let mut steps = Vec::with_capacity(n);
    for _ in 0..n {
        steps.push(Affine { a: r.f64()?, b: r.f64()?, c: r.f64()?, d: r.f64()?, e: r.f64()?, f: r.f64()? });
    }
    Some(StabilizerPath { w: w as usize, h: h as usize, steps, fps })
}

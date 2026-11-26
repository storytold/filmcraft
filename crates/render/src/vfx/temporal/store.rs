//! Warp Stabilizer analyses on disk, so a source's frames are analysed once, not once per session.
//!
//! One file per source and way of reading it, `<hash>.stab` in the folder [`set_dir`] names (the
//! engine points it into the media cache). A file holds the identity bytes it was made for (see
//! [`crate::vfx::AnalysisSpan`]), followed by the analysed ranges of the source's frames:
//!
//! ```text
//! magic "FCSTAB" 0 2 | identity length u32 | identity | width u32 | height u32 | fps f64
//! | ranges u32 | ranges × (first frame i64 | steps u32 | steps × (a b c d e f) f64)
//! ```
//!
//! (little-endian; ranges in order, none sharing a frame). The file is input like any other: a
//! short, long, foreign or corrupt one is ignored (its frames are analysed again and the file
//! replaced), and the identity is compared in full, so two identities with the same file name
//! never share an analysis. Files are written whole under a temporary name and renamed, so a
//! crash leaves no half-written analysis behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use filmcraft_geom::Affine;

use super::ranges::{MAX_SOURCE_STEPS, Range, Ranges};

/// Format and tracker version: bump it when the file layout or the analysis changes, so paths
/// made by an older tracker are made again.
const MAGIC: &[u8; 8] = b"FCSTAB\x00\x02";
/// Most ranges a file may hold.
const MAX_RANGES: usize = 1 << 16;
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

/// The analyses kept for `identity`, if there is a valid file.
pub(crate) fn load(identity: &[u8]) -> Option<Ranges> {
    let path = file(&dir()?, identity);
    let max = MAGIC.len() + 4 + MAX_IDENTITY + 4 + 4 + 8 + 4 + MAX_RANGES * 12 + MAX_SOURCE_STEPS * 48;
    if std::fs::metadata(&path).ok()?.len() > max as u64 {
        return None;
    }
    decode(&std::fs::read(&path).ok()?, identity)
}

/// Keep `ranges` as the analyses of `identity`. Failing to write them (read-only or full disk)
/// only means their frames are analysed again next time.
pub(crate) fn save(identity: &[u8], ranges: &Ranges) {
    let Some(dir) = dir() else { return };
    let Some(bytes) = encode(identity, ranges) else { return };
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

pub(crate) fn encode(identity: &[u8], ranges: &Ranges) -> Option<Vec<u8>> {
    let (w, h) = ranges.dims?;
    if identity.len() > MAX_IDENTITY || ranges.ranges.len() > MAX_RANGES || ranges.steps() > MAX_SOURCE_STEPS {
        return None;
    }
    let mut b = Vec::with_capacity(MAGIC.len() + identity.len() + 28 + ranges.ranges.len() * 12 + ranges.steps() * 48);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&(identity.len() as u32).to_le_bytes());
    b.extend_from_slice(identity);
    b.extend_from_slice(&u32::try_from(w).ok()?.to_le_bytes());
    b.extend_from_slice(&u32::try_from(h).ok()?.to_le_bytes());
    b.extend_from_slice(&ranges.fps.to_le_bytes());
    b.extend_from_slice(&(ranges.ranges.len() as u32).to_le_bytes());
    for r in &ranges.ranges {
        b.extend_from_slice(&r.start.to_le_bytes());
        b.extend_from_slice(&(r.steps.len() as u32).to_le_bytes());
        for m in &r.steps {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                b.extend_from_slice(&v.to_le_bytes());
            }
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

pub(crate) fn decode(bytes: &[u8], identity: &[u8]) -> Option<Ranges> {
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
    let count = r.u32()? as usize;
    if count > MAX_RANGES {
        return None;
    }
    let mut ranges = Vec::with_capacity(count);
    let (mut total, mut after) = (0usize, None::<i64>);
    for _ in 0..count {
        let start = i64::from_le_bytes(r.take(8)?.try_into().ok()?);
        let n = r.u32()? as usize;
        total = total.checked_add(n)?;
        // in order, none sharing a frame, at least one step each
        if n == 0 || total > MAX_SOURCE_STEPS || after.is_some_and(|a| start <= a) || r.0.len() < n.checked_mul(48)? {
            return None;
        }
        let mut steps = Vec::with_capacity(n);
        for _ in 0..n {
            steps.push(Affine { a: r.f64()?, b: r.f64()?, c: r.f64()?, d: r.f64()?, e: r.f64()?, f: r.f64()? });
        }
        let range = Range { start, steps };
        after = Some(range.last()?);
        ranges.push(range);
    }
    if !r.0.is_empty() {
        return None;
    }
    Some(Ranges { dims: Some((w as usize, h as usize)), fps, ranges })
}

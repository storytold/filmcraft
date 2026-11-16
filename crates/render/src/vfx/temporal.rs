//! Effects that look at other frames: Posterize Time, Echo, Auto Reframe and Warp Stabilizer.
//!
//! Warp Stabilizer is a 2-D (similarity / translation) stabilizer: on first use it analyses the
//! clip — every frame decoded small, Shi–Tomasi features tracked frame to frame with pyramidal
//! Lucas–Kanade and a RANSAC similarity fit (the mask tracker in [`crate::track`]) — and caches
//! the per-frame steps in memory, and on disk for media clips ([`set_stabilizer_cache_dir`]).
//! Rendering smooths the accumulated camera path with a Gaussian (Smoothness) or holds it fixed
//! (No Motion), warps each frame onto the smoothed path and frames the result (crop / auto-scale /
//! synthesise edges). Perspective and Subspace Warp use the
//! similarity model (no per-region warping).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use filmcraft_project::{EffectInstance, Project, Sequence, TrackItem, TrackMethod};
use rayon::prelude::*;

use super::{Affine, FxCtx, FxEnv, Image, Vec2, affine_warp, bv, chv, fv, luma, offv, over};
use crate::track::{Gray, Prepared, track_step};
use ranges::{Range, Ranges};

pub(super) mod ranges;
pub(super) mod store;
pub use store::set_dir as set_stabilizer_cache_dir;

/// Posterize Time: the clip shows a new frame only at the effect's frame rate.
pub fn posterize_time(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(env) = cx.env else { return };
    let rate = fv(e, "rate", cx).max(0.01) as f64;
    let t = cx.seconds;
    let q = (t * rate + 1e-6).floor() / rate;
    let dt = q - t;
    if dt.abs() < 1e-6 {
        return;
    }
    if let Some(f) = env.frame(dt, e) {
        *img = f;
    }
}

/// Echo: composites copies of the clip from earlier (negative Echo Time) or later frames.
pub fn echo(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(env) = cx.env else { return };
    let dt = fv(e, "time", cx) as f64;
    let n = fv(e, "count", cx).clamp(0.0, 30.0) as usize;
    if n == 0 || dt.abs() < 1e-6 {
        return;
    }
    let start = fv(e, "start", cx).clamp(0.0, 1.0);
    let decay = fv(e, "decay", cx).max(0.0);
    let op = chv(e, "operator");
    let mut layers: Vec<(Image, f32)> = vec![(img.clone(), start)];
    for k in 1..=n {
        if let Some(f) = env.frame(dt * k as f64, e) {
            layers.push((f, start * decay.powi(k as i32)));
        }
    }
    *img = combine_echoes(&layers, op);
}

/// Combine echo layers (index 0 = the current frame) with an Echo Operator.
pub(crate) fn combine_echoes(layers: &[(Image, f32)], op: u32) -> Image {
    let (w, h) = (layers[0].0.w, layers[0].0.h);
    let mut out = Image::new(w, h);
    let wsum: f32 = layers.iter().map(|l| l.1).sum::<f32>().max(1e-6);
    out.px.par_chunks_mut(4).enumerate().for_each(|(i, o)| {
        let px = |l: &(Image, f32)| -> [f32; 4] {
            if l.0.w != w || l.0.h != h {
                return [0.0; 4];
            }
            let p = &l.0.px[i * 4..i * 4 + 4];
            [p[0] * l.1, p[1] * l.1, p[2] * l.1, p[3] * l.1]
        };
        let mut acc = match op {
            2 => [f32::INFINITY; 4],
            3 => [1.0; 4],
            _ => [0.0; 4],
        };
        match op {
            4 => {
                // composite in back: the current frame in front, older echoes behind
                for l in layers.iter().rev() {
                    over(&mut acc, px(l));
                }
            }
            5 => {
                // composite in front: echoes over the current frame
                for l in layers {
                    over(&mut acc, px(l));
                }
            }
            _ => {
                for l in layers {
                    let p = px(l);
                    for k in 0..4 {
                        acc[k] = match op {
                            1 => acc[k].max(p[k]),
                            2 => acc[k].min(p[k]),
                            3 => acc[k] * (1.0 - p[k].min(1.0)),
                            6 => acc[k] + p[k] / wsum,
                            _ => acc[k] + p[k],
                        };
                    }
                }
                if op == 3 {
                    acc = acc.map(|v| 1.0 - v);
                }
            }
        }
        if op == 0 {
            acc[3] = acc[3].min(1.0);
        }
        o.copy_from_slice(&acc.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }));
    });
    out
}

// ------------------------------------------------------------------ analysis helpers

fn gray_of(img: &Image) -> Gray {
    let px = img.px.par_chunks(4).map(|p| filmcraft_color::linear_to_srgb(luma([p[0], p[1], p[2]]).clamp(0.0, 1.0))).collect();
    Gray::new(img.w, img.h, px)
}

/// Saliency centroid (0..1 of the frame): gradient energy weighted, mildly centre-biased.
pub(crate) fn saliency_centroid(img: &Image) -> (f32, f32) {
    let g = gray_of(img);
    let (w, h) = (g.w, g.h);
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let gx = g.px[y * w + x + 1] - g.px[y * w + x - 1];
            let gy = g.px[(y + 1) * w + x] - g.px[(y - 1) * w + x];
            let (u, v) = (x as f32 / w as f32 - 0.5, y as f32 / h as f32 - 0.5);
            let bias = 1.0 - (u * u + v * v) * 0.8;
            let e = ((gx * gx + gy * gy) * bias.max(0.1)) as f64;
            let e = e * e;
            sx += e * x as f64;
            sy += e * y as f64;
            sw += e;
        }
    }
    if sw <= 1e-12 {
        return (0.5, 0.5);
    }
    (((sx / sw + 0.5) / w as f64) as f32, ((sy / sw + 0.5) / h as f64) as f32)
}

type CentroidCache = Mutex<HashMap<(u64, i64), (f32, f32)>>;
fn centroid_cache() -> &'static CentroidCache {
    static C: OnceLock<CentroidCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Auto Reframe: keeps the subject (a saliency centroid averaged over a time window set by the
/// Motion Preset) in frame while zooming the clip to fill the sequence (or a chosen aspect).
pub fn auto_reframe(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let (lw, lh) = (img.w as f64, img.h as f64);
    // the frame the layer must fill, in layer pixels (Motion at its defaults places the layer
    // centred in the sequence 1:1)
    let target = match (chv(e, "aspect"), cx.env) {
        (0, Some(env)) => {
            let (sw, sh) = env.sequence_size();
            let (src_w, _) = env.source_size();
            let k = lw / src_w.max(1) as f64;
            (sw as f64 * k, sh as f64 * k)
        }
        (0, None) => return,
        (a, _) => {
            let r = [16.0 / 9.0, 9.0 / 16.0, 1.0, 4.0 / 5.0, 16.0 / 9.0][a.min(4) as usize];
            if lw / lh > r { (lh * r, lh) } else { (lw, lw / r) }
        }
    };
    let zoom_user = (fv(e, "zoom", cx) as f64 / 100.0).max(1.0);
    let z = (target.0 / lw).max(target.1 / lh).max(1.0) * zoom_user;
    // window of the source that stays visible
    let (vw, vh) = (target.0.min(lw * z) / z, target.1.min(lh * z) / z);
    let (half, step) = [(1.0, 0.25), (0.5, 0.125), (0.15, 0.05)][chv(e, "preset").min(2) as usize];
    let subject = match cx.env {
        Some(env) => {
            let key = env.clip_key();
            let fps = env.frame_rate().max(1.0);
            let mut acc = (0.0f64, 0.0f64, 0.0f64);
            let mut k = -half;
            while k <= half + 1e-9 {
                let t = cx.seconds + k;
                if t >= -1e-9 && t <= env.clip_seconds() + 1e-9 {
                    let id = (t * fps).round() as i64;
                    let cached = centroid_cache().lock().ok().and_then(|c| c.get(&(key, id)).copied());
                    let c = cached.or_else(|| {
                        let f = env.source_frame(id as f64 / fps - cx.seconds, (160.0 / env.source_size().0.max(1) as f32).min(1.0))?;
                        let c = saliency_centroid(&f);
                        if let Ok(mut m) = centroid_cache().lock() {
                            if m.len() > 100_000 {
                                m.clear();
                            }
                            m.insert((key, id), c);
                        }
                        Some(c)
                    });
                    if let Some((u, v)) = c {
                        let wgt = 1.0 - (k / (half + step)).abs();
                        acc = (acc.0 + u as f64 * wgt, acc.1 + v as f64 * wgt, acc.2 + wgt);
                    }
                }
                k += step;
            }
            if acc.2 > 0.0 { (acc.0 / acc.2, acc.1 / acc.2) } else { (0.5, 0.5) }
        }
        None => {
            let (u, v) = saliency_centroid(&img.downsample2());
            (u as f64, v as f64)
        }
    };
    let off = offv(e, "offset", cx);
    let sx = (subject.0 * lw + off.x).clamp(vw / 2.0, lw - vw / 2.0);
    let sy = (subject.1 * lh + off.y).clamp(vh / 2.0, lh - vh / 2.0);
    // output(p) = input(subject + (p − centre) / z)
    let m = Affine::translate(lw / 2.0, lh / 2.0).then_apply(&Affine::scale(z, z)).then_apply(&Affine::translate(-sx, -sy));
    if (z - 1.0).abs() < 1e-9 && (sx - lw / 2.0).abs() < 1e-6 && (sy - lh / 2.0).abs() < 1e-6 {
        return;
    }
    affine_warp(img, &m);
}

// ------------------------------------------------------------------ Warp Stabilizer

/// A clip's analysed camera motion: per-frame steps (frame k → k+1) in analysis pixels.
#[derive(Clone, Debug)]
pub struct StabilizerPath {
    pub w: usize,
    pub h: usize,
    pub steps: Vec<Affine>,
    pub fps: f64,
}

/// Most frames of a clip an analysis reads (5½ minutes at 60 fps); a longer clip's later frames
/// keep the last step.
pub(crate) const MAX_ANALYSIS_FRAMES: usize = 20_000;

/// Frames read, prepared and tracked together. The analysis holds two blocks (the one being
/// tracked and the next one being read) and one frame carried over, whatever the clip's length.
pub(super) const ANALYSIS_BLOCK: usize = 16;

/// How far the analysis of a clip has got: frames `0..next` are read and the steps between them
/// known (`steps.len() == next - 1`). Kept when the analysis is cancelled, so the next request
/// goes on from there.
#[derive(Default)]
pub(super) struct Progress {
    steps: Vec<Affine>,
    dims: Option<(usize, usize)>,
    next: usize,
}

impl Progress {
    /// Work out the steps of the rest of the `n` frames, then return true; false when cancelled
    /// first (what was done is kept). `decode` makes frame `k` (`None` when it can't be read: its
    /// steps are the identity), `size` is a frame's (width, height), `step` the motion from one
    /// frame to the next given the size of the first frame that could be read.
    ///
    /// Frames are read one at a time, in order: they come from one decoder per source, and asked
    /// for many at once it decodes each from its keyframe again (a 4K HEVC clip decoded every
    /// sample twenty times over, #291). The steps of a block are tracked in parallel while the
    /// next block is read, and its frames are dropped once they're known; only its last frame
    /// stays for the next block.
    pub(super) fn advance<T: Send + Sync>(
        &mut self,
        n: usize,
        block: usize,
        decode: impl Fn(usize) -> Option<T> + Sync,
        size: impl Fn(&T) -> (usize, usize) + Sync,
        step: impl Fn(&T, &T, (usize, usize)) -> Affine + Sync,
        cancelled: impl Fn() -> bool + Sync,
    ) -> bool {
        let block = block.max(1);
        // frames `from..to`, fewer when cancelled meanwhile (then false)
        let read = |from: usize, to: usize| -> (Vec<Option<T>>, bool) {
            let mut frames = Vec::with_capacity(to.saturating_sub(from));
            for k in from..to {
                if cancelled() {
                    return (frames, false);
                }
                frames.push(decode(k));
            }
            (frames, true)
        };
        // the frame before `next`: its step to `next` isn't known yet
        let mut carried: Option<Option<T>> = None;
        if self.next > 0 && self.next < n {
            if cancelled() {
                return false;
            }
            carried = Some(decode(self.next - 1));
        }
        let mut ahead: Option<(Vec<Option<T>>, bool)> = None;
        while self.next < n {
            let (fresh, whole) = match ahead.take() {
                Some(f) => f,
                None => read(self.next, self.next.saturating_add(block).min(n)),
            };
            if fresh.is_empty() {
                return false;
            }
            // A request cancelled part way through a block still moves the analysis on by the
            // frames it read: scrubbing cancels requests long before a block is read.
            let end = self.next.saturating_add(fresh.len()).min(n);
            let mut frames: Vec<Option<T>> = Vec::with_capacity(fresh.len() + 1);
            frames.extend(carried.take());
            frames.extend(fresh);
            if self.dims.is_none() {
                self.dims = frames.iter().flatten().next().map(&size);
            }
            let dims = self.dims;
            let (pairs, next_block) = rayon::join(
                || {
                    (0..frames.len().saturating_sub(1))
                        .into_par_iter()
                        .map(|i| match (frames.get(i), frames.get(i + 1), dims) {
                            (Some(Some(a)), Some(Some(b)), Some(d)) if size(a).0 == size(b).0 => step(a, b, d),
                            _ => Affine::IDENTITY,
                        })
                        .collect::<Vec<Affine>>()
                },
                || if whole && end < n { Some(read(end, end.saturating_add(block).min(n))) } else { None },
            );
            self.steps.extend(pairs);
            carried = frames.pop();
            self.next = end;
            if !whole {
                return false;
            }
            ahead = next_block;
        }
        true
    }

    /// The steps and the frame size of a finished analysis; `None` when no frame could be read.
    #[cfg(test)]
    pub(super) fn finished(self) -> Option<(Vec<Affine>, (usize, usize))> {
        Some((self.steps, self.dims?))
    }
}

/// [`Progress::advance`] over a whole clip: the steps and the frame size, `None` when cancelled
/// or when no frame could be read.
#[cfg(test)]
pub(super) fn chunked_steps<T: Send + Sync>(
    n: usize,
    block: usize,
    decode: impl Fn(usize) -> Option<T> + Sync,
    size: impl Fn(&T) -> (usize, usize) + Sync,
    step: impl Fn(&T, &T, (usize, usize)) -> Affine + Sync,
    cancelled: impl Fn() -> bool + Sync,
) -> Option<(Vec<Affine>, (usize, usize))> {
    let mut p = Progress::default();
    if !p.advance(n, block, decode, size, step, cancelled) {
        return None;
    }
    p.finished()
}

/// Every clip's analysis, and which one is running. One runs at a time: clips of the same file
/// share its decoder, and two analyses reading different parts of it at once would each send it
/// back to a keyframe for every frame.
#[derive(Default)]
struct Analyses {
    /// Finished paths, by clip key.
    clips: HashMap<u64, Arc<StabilizerPath>>,
    /// What is analysed of each source (finished or not), by source key: clips read their path
    /// from here, and an analysis stopped part way keeps what it did here.
    sources: HashMap<u64, Ranges>,
    /// Sources whose analyses were looked for on disk.
    looked_up: HashSet<u64>,
    /// Clips whose analysis finished without a frame it could read: frames rendered without
    /// waiting show them unstabilised instead of asking for the analysis again and again.
    unreadable: HashSet<u64>,
    /// The clip key of the running analysis.
    running: Option<u64>,
    /// Bumped by [`clear_stabilizer_cache`]: an analysis that started before isn't kept.
    generation: u64,
}

fn analyses() -> &'static (Mutex<Analyses>, Condvar) {
    static A: OnceLock<(Mutex<Analyses>, Condvar)> = OnceLock::new();
    A.get_or_init(Default::default)
}

fn lock_analyses() -> MutexGuard<'static, Analyses> {
    analyses().0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Most finished clip paths kept in memory; each is a few hundred bytes a second.
const KEPT_ANALYSES: usize = 64;
/// Most steps of source analyses kept in memory (48 MB); beyond, they are read from disk again.
const KEPT_STEPS: usize = 1 << 20;

/// How often a request waiting for an analysis checks whether it was cancelled.
const WAIT_POLL: Duration = Duration::from_millis(10);

/// Threads that run the analyses. A request that waits for an analysis may be a rayon worker of
/// the pool that renders frames (an export renders a batch on all of them). Had the analysis run
/// on that pool, a worker waiting for it could hold part of its parallel work on its stack (a
/// worker waiting inside a join runs other jobs, such as another frame that asks for the same
/// analysis) and it would never finish (#2276). On its own pool, its work only runs beside other
/// work of the analysis, which never waits for an analysis.
#[cfg(not(target_arch = "wasm32"))]
fn analysis_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map(std::num::NonZeroUsize::get).unwrap_or(1);
        rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("stabilize-{i}")).build().ok()
    })
    .as_ref()
}

/// Forget every cached stabilizer analysis (e.g. after the media changed).
pub fn clear_stabilizer_cache() {
    let mut a = lock_analyses();
    a.clips.clear();
    a.sources.clear();
    a.looked_up.clear();
    a.unreadable.clear();
    a.generation = a.generation.wrapping_add(1);
}

/// Analyse (or fetch the cached analysis of) the clip `env` describes. Frames that ask for the
/// same clip at once share one analysis. A request that is cancelled while it waits returns
/// `None`; the analysis it was running stops, keeps what it did, and the next request for the
/// clip goes on from there.
///
/// Clips of one source read the same way share what is analysed of it ([`FxEnv::analysis_span`]):
/// a trimmed or cut clip reads its path from what was analysed before, and an extended one
/// analyses only its new frames. A media clip's analyses are also kept on disk
/// ([`set_stabilizer_cache_dir`]) and read from there by later sessions. Inside
/// [`without_waiting_for_analysis`] a clip that isn't analysed yet returns `None` at once and is
/// reported to the caller instead.
pub fn stabilizer_path(env: &dyn FxEnv, _now: f64, method: TrackMethod, detailed: bool) -> Option<Arc<StabilizerPath>> {
    let fps = env.frame_rate().max(1.0);
    let key = analysis_key(env.analysis_span(), env.clip_key(), method, detailed, analysis_frames(env.clip_seconds(), fps));
    let (a, done) = finished(lock_analyses(), &key);
    if done.is_some() {
        return done;
    }
    let mut a = a;
    if LIVE.with(Cell::get) {
        if a.unreadable.contains(&key.clip) {
            return None;
        }
        WAITING.with(|w| {
            let mut w = w.borrow_mut();
            if !w.contains(&key.clip) {
                w.push(key.clip);
            }
        });
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let pool = analysis_pool();
    #[cfg(target_arch = "wasm32")]
    let pool: Option<&rayon::ThreadPool> = None;
    // A nested sequence's clips are rendered for the frames of an analysis: one of them asking
    // for its own analysis must not wait for the one that is reading it. Without the pool (the
    // web build, or no threads) nothing is shared either.
    let Some(pool) = pool.filter(|p| p.current_thread_index().is_none()) else {
        let todo = Todo::of(&a, &key);
        drop(a);
        let (runs, complete) = analyse(env, method, detailed, &todo, &filmcraft_media::cancel::cancelled, None);
        return keep(lock_analyses(), &key, None, runs, fps, complete);
    };
    let yielding = YIELDING.with(Cell::get);
    {
        // a request that waits counts, unless it is one that gives way itself
        let _waiting = (!yielding).then(Waiting::new);
        loop {
            let (g, done) = finished(a, &key);
            a = g;
            if done.is_some() {
                return done;
            }
            if yielding && WAITERS.load(Ordering::Relaxed) > 0 {
                GAVE_WAY.with(|g| g.set(true));
                return None;
            }
            if a.running.is_none() {
                break;
            }
            if filmcraft_media::cancel::cancelled() {
                return None;
            }
            a = analyses().1.wait_timeout(a, WAIT_POLL).map_or_else(|e| e.into_inner().0, |(g, _)| g);
        }
    }
    let todo = Todo::of(&a, &key);
    a.running = Some(key.clip);
    let generation = a.generation;
    drop(a);
    let mut turn = Turn { ended: false };
    let (runs, complete) = analyse_on(pool, env, method, detailed, &todo, yielding);
    let mut a = lock_analyses();
    // the waiting requests see the analysis' result when they get the lock back
    a.running = None;
    RUNNING_TOTAL.store(0, Ordering::Relaxed);
    turn.ended = true;
    analyses().1.notify_all();
    keep(a, &key, Some(generation), runs, fps, complete)
}

/// Which analysis a clip needs.
pub(super) struct Key {
    /// The clip's own (its finished path, whether it is running or unreadable).
    pub(super) clip: u64,
    /// Its source's, shared by the clips that read the same frames of it the same way.
    source: u64,
    /// The source's identity on disk (media clips only).
    identity: Option<Vec<u8>>,
    /// The source frame of the clip's first frame, and how many frames the clip has.
    first: i64,
    n: usize,
}

/// The keys of the analysis of `n` frames of a clip by `method` (and `detailed`).
fn analysis_key(span: Option<super::AnalysisSpan>, clip_key: u64, method: TrackMethod, detailed: bool, n: usize) -> Key {
    match span {
        Some(super::AnalysisSpan { mut source, first }) => {
            source.extend([method as u8, detailed as u8]);
            let mut clip = source.clone();
            clip.extend(first.to_le_bytes());
            clip.extend((n as u64).to_le_bytes());
            Key { clip: store::fnv(&clip), source: store::fnv(&source), identity: Some(source), first, n }
        }
        None => {
            let k = clip_key ^ (method as u64).wrapping_mul(0x9E37) ^ (detailed as u64).wrapping_mul(0x51_7CC1);
            Key { clip: k, source: k, identity: None, first: 0, n }
        }
    }
}

/// The finished path of the clip `key`: kept, or read from its source's analyses (in memory, or
/// looked for once per source on disk).
fn finished(mut a: MutexGuard<'static, Analyses>, key: &Key) -> (MutexGuard<'static, Analyses>, Option<Arc<StabilizerPath>>) {
    if let Some(p) = a.clips.get(&key.clip) {
        let p = p.clone();
        return (a, Some(p));
    }
    if let Some(id) = key.identity.as_deref()
        && !a.looked_up.contains(&key.source)
    {
        drop(a);
        let found = store::load(id);
        a = lock_analyses();
        a.looked_up.insert(key.source);
        if let Some(found) = found {
            make_room(&mut a, key.source, found.steps());
            let kept = a.sources.entry(key.source).or_insert_with(|| Ranges { dims: found.dims, fps: found.fps, ranges: Vec::new() });
            // what this session analysed already stays; the file adds to it
            if kept.dims.is_none() {
                kept.dims = found.dims;
            }
            if kept.dims == found.dims {
                for r in found.ranges {
                    kept.insert(r);
                }
            }
        }
    }
    let path = a.sources.get(&key.source).and_then(|r| r.path(key.first, key.n)).map(Arc::new);
    if let Some(p) = &path {
        if a.clips.len() >= KEPT_ANALYSES {
            a.clips.clear();
            a.unreadable.clear();
        }
        a.clips.insert(key.clip, p.clone());
    }
    (a, path)
}

/// Keep the source analyses in memory under [`KEPT_STEPS`] when `adding` more steps to `source`:
/// the others go (those of media clips are read from disk again when needed).
fn make_room(a: &mut Analyses, source: u64, adding: usize) {
    let kept: usize = a.sources.values().map(Ranges::steps).sum();
    if kept.saturating_add(adding) > KEPT_STEPS {
        a.sources.retain(|k, _| *k == source);
        a.looked_up.retain(|k| *k == source);
    }
}

/// The frames an analysis of a clip still has to read.
struct Todo {
    n: usize,
    /// Runs of clip frames `a..=b` to read, and whether the first of each was analysed already.
    runs: Vec<(usize, usize, bool)>,
    /// Frames of the clip analysed already.
    known: usize,
}

impl Todo {
    fn of(a: &Analyses, key: &Key) -> Todo {
        let n = key.n.max(1);
        let Some(r) = a.sources.get(&key.source) else { return Todo { n, runs: vec![(0, n - 1, false)], known: 0 } };
        let mut runs: Vec<(usize, usize, bool)> =
            r.gaps(key.first, n).into_iter().map(|(from, to)| (from, to, key.first.checked_add(from as i64).is_some_and(|f| r.known(f, 1) == 1))).collect();
        if runs.is_empty() {
            // nothing missing yet no path (no frame size known): read it all
            runs.push((0, n - 1, false));
        }
        Todo { n, runs, known: r.known(key.first, n) }
    }
}

/// Requests waiting for another request's analysis to end before theirs can run (renders that
/// wait: exports, Render In to Out). Analyses run [`giving_way`] stop for them.
static WAITERS: AtomicUsize = AtomicUsize::new(0);

/// One request counted in [`WAITERS`] while it lives.
struct Waiting;

impl Waiting {
    fn new() -> Waiting {
        WAITERS.fetch_add(1, Ordering::Relaxed);
        Waiting
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITERS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Run `f` with its analyses giving way: one stops (keeping what it read) as soon as a render is
/// waiting for its turn, and none starts while one is. For analyses nobody is looking at yet.
pub fn giving_way<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            YIELDING.with(|y| y.set(self.0));
        }
    }
    let _restore = Restore(YIELDING.with(|y| y.replace(true)));
    f()
}

/// Whether a render is waiting for an analysis (an analysis [`giving_way`] would not start now).
pub fn analysis_wanted_elsewhere() -> bool {
    WAITERS.load(Ordering::Relaxed) > 0
}

/// The analyses an application has lined up, in the order it runs them, and who lined them up.
fn queue() -> &'static Mutex<(u64, Vec<u64>)> {
    static Q: Mutex<(u64, Vec<u64>)> = Mutex::new((0, Vec::new()));
    &Q
}

/// Publish the analyses `owner` has lined up (clip keys, in order) for [`stabilizer_state`].
pub fn set_analysis_queue(owner: u64, keys: Vec<u64>) {
    *queue().lock().unwrap_or_else(PoisonError::into_inner) = (owner, keys);
}

/// Withdraw `owner`'s queue (it is gone); another owner's stays.
pub fn clear_analysis_queue(owner: u64) {
    let mut q = queue().lock().unwrap_or_else(PoisonError::into_inner);
    if q.0 == owner {
        *q = (0, Vec::new());
    }
}

/// Run `f` and say whether an analysis it ran gave way to a waiting render ([`giving_way`]).
pub(super) fn noting_give_way<R>(f: impl FnOnce() -> R) -> (R, bool) {
    let before = GAVE_WAY.with(|g| g.replace(false));
    let r = f();
    let gave = GAVE_WAY.with(|g| g.replace(before));
    (r, gave)
}

thread_local! {
    /// Inside [`giving_way`].
    static YIELDING: Cell<bool> = const { Cell::new(false) };
    /// An analysis run [`giving_way`] stopped (or didn't start) for a waiting render.
    static GAVE_WAY: Cell<bool> = const { Cell::new(false) };
    /// Inside [`without_waiting_for_analysis`].
    static LIVE: Cell<bool> = const { Cell::new(false) };
    /// The analyses frames rendered inside it went without.
    static WAITING: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// Run `f` (a render) without waiting for Warp Stabilizer analyses: a clip whose analysis isn't
/// finished renders unstabilised, and the keys of the analyses it went without are returned
/// (empty when every frame came out finished). For monitors: the frame is shown at once, and
/// rendered again once [`stabilizer_analysed`] says its analysis is done. Exports and renders
/// outside it wait for the analysis, as before.
///
/// Only the calling thread renders without waiting; work `f` hands to other threads waits.
pub fn without_waiting_for_analysis<R>(f: impl FnOnce() -> R) -> (R, Vec<u64>) {
    struct Restore(bool, Vec<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIVE.with(|l| l.set(self.0));
            WAITING.with(|w| *w.borrow_mut() = std::mem::take(&mut self.1));
        }
    }
    let restore = Restore(LIVE.with(|l| l.replace(true)), WAITING.with(|w| std::mem::take(&mut *w.borrow_mut())));
    let r = f();
    let waiting = WAITING.with(|w| std::mem::take(&mut *w.borrow_mut()));
    drop(restore);
    (r, waiting)
}

/// Forget the analysis of `env`'s clip and source in memory, as a new session would (the disk
/// copy stays).
#[cfg(test)]
pub(super) fn forget_analysis(env: &dyn FxEnv, method: TrackMethod, detailed: bool) {
    let key = analysis_key(env.analysis_span(), env.clip_key(), method, detailed, analysis_frames(env.clip_seconds(), env.frame_rate()));
    let mut a = lock_analyses();
    a.clips.remove(&key.clip);
    a.unreadable.remove(&key.clip);
    a.sources.remove(&key.source);
    a.looked_up.remove(&key.source);
}

/// Whether the analysis of the Warp Stabilizer `e` on `item` is settled without running one: kept
/// (in memory or on disk), or found to have no frame to read.
pub(super) fn ready(project: &Project, seq: &Sequence, item: &TrackItem, e: &EffectInstance) -> bool {
    let key = effect_key(project, seq, item, e);
    let (a, done) = finished(lock_analyses(), &key);
    done.is_some() || a.unreadable.contains(&key.clip)
}

/// The clip key of the Warp Stabilizer `e` on `item`, for tests.
#[cfg(test)]
pub(super) fn key_of_effect(project: &Project, seq: &Sequence, item: &TrackItem, e: &EffectInstance) -> u64 {
    effect_key(project, seq, item, e).clip
}

/// [`in_size`], for tests.
#[cfg(test)]
pub(super) fn steps_in_size(steps: Vec<Affine>, from: (usize, usize), to: (usize, usize)) -> Vec<Affine> {
    in_size(steps, from, to)
}

/// The finished path of the Warp Stabilizer `e` on `item`, as frames rendered now would use it.
#[cfg(test)]
pub(super) fn path_of(project: &Project, seq: &Sequence, item: &TrackItem, e: &EffectInstance) -> Option<Arc<StabilizerPath>> {
    finished(lock_analyses(), &effect_key(project, seq, item, e)).1
}

/// The key [`stabilizer_path`] uses for `env`.
#[cfg(test)]
pub(super) fn key_of(env: &dyn FxEnv, method: TrackMethod, detailed: bool) -> u64 {
    analysis_key(env.analysis_span(), env.clip_key(), method, detailed, analysis_frames(env.clip_seconds(), env.frame_rate())).clip
}

/// Whether the analysis `key` (from [`without_waiting_for_analysis`] or [`stabilizer_keys`]) is
/// finished and in memory (or found no frame it could read: the clip stays unstabilised).
pub fn stabilizer_analysed(key: u64) -> bool {
    let a = lock_analyses();
    a.clips.contains_key(&key) || a.unreadable.contains(&key)
}

/// Frames the running analysis has read, and how many it reads (0: none is running).
static RUNNING_DONE: AtomicUsize = AtomicUsize::new(0);
static RUNNING_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// How far the running analysis has got (0 to 1), `None` when none is running.
pub fn stabilizer_progress() -> Option<f32> {
    let total = RUNNING_TOTAL.load(Ordering::Relaxed);
    (total > 0).then(|| (RUNNING_DONE.load(Ordering::Relaxed) as f32 / total as f32).clamp(0.0, 1.0))
}

/// The analysis keys of every Warp Stabilizer in `project` (every sequence, nested ones too), as
/// [`without_waiting_for_analysis`] reports them: an analysis whose key isn't among them is no
/// longer wanted.
pub fn stabilizer_keys(project: &Project) -> HashSet<u64> {
    let mut keys = HashSet::new();
    for p in project.sequences() {
        let filmcraft_project::ItemKind::Sequence(seq) = &p.kind else { continue };
        for item in seq.video_tracks.iter().flat_map(|t| t.items.iter()) {
            for e in item.effects.iter().filter(|e| e.effect == "warp_stabilizer") {
                keys.insert(effect_key(project, seq, item, e).clip);
            }
        }
    }
    keys
}

/// The analysis keys of the Warp Stabilizer `e` on `item`.
pub(super) fn effect_key(project: &Project, seq: &Sequence, item: &TrackItem, e: &EffectInstance) -> Key {
    let method = if chv(e, "method") == 0 { TrackMethod::Position } else { TrackMethod::PositionScaleRotation };
    let n = analysis_frames(item.duration.seconds(), seq.settings.frame_rate.as_f64());
    analysis_key(super::analysis_span(project, seq, item), super::clip_key_of(item), method, bv(e, "detailed"), n)
}

/// Where the analysis of a Warp Stabilizer stands (`sequence.inspect` reports it to agents).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalysisState {
    pub phase: AnalysisPhase,
    /// Frames of the clip analysed so far (all of them once it is done).
    pub analyzed: usize,
    /// Frames of the clip the analysis reads.
    pub total: usize,
    /// Its place in line while queued (1: next).
    pub position: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisPhase {
    /// Finished, in memory or kept on disk: the clip's frames are stabilized.
    Done,
    /// Running now.
    Running,
    /// Lined up behind other analyses; it starts by itself ([`AnalysisState::position`]).
    Queued,
    /// None of these: it runs when a frame of the clip is shown or rendered, going on from the
    /// frames already analysed by an analysis that was stopped.
    Waiting,
    /// It finished without a frame it could read: the clip is shown unstabilized.
    Unreadable,
}

/// The frames an analysis of a clip `seconds` long reads at `fps`.
pub(crate) fn analysis_frames(seconds: f64, fps: f64) -> usize {
    let n = (seconds * fps.max(1.0)).round();
    if n.is_finite() && n >= 1.0 { (n as usize).min(MAX_ANALYSIS_FRAMES) } else { 1 }
}

/// The state of the analysis of the Warp Stabilizer `e` on `item` in `seq`.
pub fn stabilizer_state(project: &Project, seq: &Sequence, item: &TrackItem, e: &EffectInstance) -> AnalysisState {
    let key = effect_key(project, seq, item, e);
    let total = key.n;
    let (a, done) = finished(lock_analyses(), &key);
    let (phase, analyzed) = if done.is_some() {
        (AnalysisPhase::Done, total)
    } else if a.unreadable.contains(&key.clip) {
        (AnalysisPhase::Unreadable, total)
    } else if a.running == Some(key.clip) {
        (AnalysisPhase::Running, RUNNING_DONE.load(Ordering::Relaxed).min(total))
    } else {
        (AnalysisPhase::Waiting, a.sources.get(&key.source).map_or(0, |r| r.known(key.first, key.n)))
    };
    let mut position = None;
    let phase = if phase == AnalysisPhase::Waiting {
        // a place among the analyses still to run (not one that is running or done)
        let q = queue().lock().unwrap_or_else(PoisonError::into_inner);
        position =
            q.1.iter().filter(|k| Some(**k) != a.running && !a.clips.contains_key(k) && !a.unreadable.contains(k)).position(|k| *k == key.clip).map(|i| i + 1);
        if position.is_some() { AnalysisPhase::Queued } else { phase }
    } else {
        phase
    };
    AnalysisState { phase, analyzed, total, position }
}

/// The running analysis' turn. Ended by its request once its result is kept; ended here when that
/// request unwinds instead, or every later request would wait for it forever.
struct Turn {
    ended: bool,
}

impl Drop for Turn {
    fn drop(&mut self) {
        if !self.ended {
            lock_analyses().running = None;
            analyses().1.notify_all();
        }
    }
}

/// Record what the analysis of `key` read in its `runs` (not when the cache was cleared since
/// `generation`) and return the clip's path when it is finished.
fn keep(
    mut a: MutexGuard<'static, Analyses>,
    key: &Key,
    generation: Option<u64>,
    runs: Vec<(usize, Progress)>,
    fps: f64,
    complete: bool,
) -> Option<Arc<StabilizerPath>> {
    if generation.is_some_and(|g| g != a.generation) {
        return None;
    }
    make_room(&mut a, key.source, runs.iter().map(|(_, p)| p.steps.len()).sum());
    let kept = a.sources.entry(key.source).or_insert_with(|| Ranges { dims: None, fps, ranges: Vec::new() });
    let mut added = false;
    for (from, p) in runs {
        // no frame of the run could be read: nothing is kept, a later analysis tries again
        let Some(d) = p.dims else { continue };
        let k = *kept.dims.get_or_insert(d);
        let Some(start) = key.first.checked_add(from as i64) else { continue };
        if !p.steps.is_empty() {
            kept.insert(Range { start, steps: in_size(p.steps, d, k) });
            added = true;
        }
    }
    let path = kept.path(key.first, key.n).filter(|_| complete).map(Arc::new);
    // written when an analysis ends, not each time one is stopped (moving the playhead stops one)
    let save = (added && complete && key.identity.is_some()).then(|| kept.clone());
    match &path {
        Some(p) => {
            if a.clips.len() >= KEPT_ANALYSES {
                a.clips.clear();
                a.unreadable.clear();
            }
            a.clips.insert(key.clip, p.clone());
        }
        // read to the end without a frame it could read: the clip stays unstabilised
        None if complete => {
            a.unreadable.insert(key.clip);
        }
        None => {}
    }
    drop(a);
    if let (Some(id), Some(ranges)) = (key.identity.as_deref(), save) {
        store::save(id, &ranges);
    }
    path
}

/// `steps` measured on frames of size `from`, as measured on frames of size `to`: a source's proxy
/// and original, read the same way, can decode to frames of different sizes.
fn in_size(steps: Vec<Affine>, from: (usize, usize), to: (usize, usize)) -> Vec<Affine> {
    if from == to {
        return steps;
    }
    let (sx, sy) = (to.0 as f64 / from.0.max(1) as f64, to.1 as f64 / from.1.max(1) as f64);
    let (s, back) = (Affine::scale(sx, sy), Affine::scale(1.0 / sx, 1.0 / sy));
    steps.iter().map(|m| s.then_apply(m).then_apply(&back)).collect()
}

/// Run [`analyse`] on the analysis pool, stopping it when this request is cancelled. The request
/// waits on a thread of its own, which takes no other rayon work meanwhile (see [`analysis_pool`]).
fn analyse_on(pool: &rayon::ThreadPool, env: &dyn FxEnv, method: TrackMethod, detailed: bool, todo: &Todo, yielding: bool) -> (Vec<(usize, Progress)>, bool) {
    let stop = AtomicBool::new(false);
    let gave_way = AtomicBool::new(false);
    let started = AtomicBool::new(false);
    let run = || {
        started.store(true, Ordering::Relaxed);
        let stopped = || stop.load(Ordering::Relaxed);
        // a panic in the analysis loses it, not the frame that asked: the frame is not stabilised
        let report = Some(&RUNNING_DONE);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool.install(|| analyse(env, method, detailed, todo, &stopped, report)))).ok()
    };
    let done = std::thread::scope(|scope| {
        let Ok(handle) = std::thread::Builder::new().name("stabilize-wait".into()).spawn_scoped(scope, run) else { return None };
        while !handle.is_finished() {
            if filmcraft_media::cancel::cancelled() {
                stop.store(true, Ordering::Relaxed);
            }
            if yielding && WAITERS.load(Ordering::Relaxed) > 0 {
                gave_way.store(true, Ordering::Relaxed);
                stop.store(true, Ordering::Relaxed);
            }
            std::thread::sleep(WAIT_POLL);
        }
        handle.join().ok().flatten()
    });
    if gave_way.load(Ordering::Relaxed) {
        GAVE_WAY.with(|g| g.set(true));
    }
    match done {
        Some(r) => r,
        // no thread could be made: analyse here (or nothing, after a panic)
        None if !started.load(Ordering::Relaxed) => analyse(env, method, detailed, todo, &filmcraft_media::cancel::cancelled, None),
        None => (Vec::new(), false),
    }
}

/// Read the frames `todo` lists and work out their steps; true when every run was read to its
/// end. Each run comes back with where it starts in the clip, however far it got.
/// Runs without the request's playback hints: a draft or catching-up decode would leave frames
/// out or degrade them in a path that is kept for every later frame.
/// `report`, when given, counts the clip's frames analysed (for [`stabilizer_progress`]).
fn analyse(
    env: &dyn FxEnv,
    method: TrackMethod,
    detailed: bool,
    todo: &Todo,
    stopped: &(dyn Fn() -> bool + Sync),
    report: Option<&AtomicUsize>,
) -> (Vec<(usize, Progress)>, bool) {
    if let Some(r) = report {
        r.store(todo.known, Ordering::Relaxed);
        RUNNING_TOTAL.store(todo.n, Ordering::Relaxed);
    }
    let src_w = env.source_size().0.max(1) as f32;
    let target_w = if detailed { 960.0 } else { 480.0 };
    let scale = (target_w / src_w).min(1.0);
    let mut runs = Vec::with_capacity(todo.runs.len());
    // frames of the clip analysed once the runs before this one are done
    let mut known = todo.known;
    for &(from, to, first_known) in &todo.runs {
        let len = to.saturating_sub(from) + 1;
        let mut progress = Progress::default();
        let complete = filmcraft_media::cancel::with_draft(false, || {
            filmcraft_media::cancel::with_catch_up(None, || {
                progress.advance(
                    len,
                    ANALYSIS_BLOCK,
                    |k| {
                        if let Some(r) = report {
                            r.store((known + k + 1 - usize::from(first_known)).min(todo.n), Ordering::Relaxed);
                        }
                        env.analysis_frame(from + k, scale).map(|f| Prepared::new(gray_of(&f)))
                    },
                    |p: &Prepared| (p.base().w, p.base().h),
                    |a: &Prepared, b: &Prepared, (w, h)| {
                        let m = 8.0;
                        let region = [Vec2::new(m, m), Vec2::new(w as f64 - m, m), Vec2::new(w as f64 - m, h as f64 - m), Vec2::new(m, h as f64 - m)];
                        track_step(a, b, &region, method).map(|s| s.transform).unwrap_or(Affine::IDENTITY)
                    },
                    stopped,
                )
            })
        });
        known += len - usize::from(first_known);
        runs.push((from, progress));
        if !complete {
            return (runs, false);
        }
    }
    (runs, true)
}

/// Similarity parameters (tx, ty, angle, log-scale) of an affine.
fn decompose(m: &Affine) -> [f64; 4] {
    let s = (m.a * m.a + m.b * m.b).sqrt().max(1e-9);
    [m.e, m.f, m.b.atan2(m.a), s.ln()]
}
fn compose(p: [f64; 4]) -> Affine {
    let s = p[3].exp();
    let (sn, cs) = p[2].sin_cos();
    Affine { a: s * cs, b: s * sn, c: -s * sn, d: s * cs, e: p[0], f: p[1] }
}

/// Widest smoothing kernel (Gaussian sigma, in frames) Warp Stabilizer applies: 50 s at 60 fps.
const MAX_SMOOTH_SIGMA: f64 = 3_000.0;

/// Per-frame correction transforms (current frame px → stabilised px, analysis pixels).
pub(crate) fn corrections(path: &StabilizerPath, smooth_frames: f64, no_motion: bool, preserve_scale: bool) -> Vec<Affine> {
    let n = path.steps.len() + 1;
    // cumulative: frame 0 → frame k, about the frame centre for stable angle/scale decomposition
    let (cxp, cyp) = (path.w as f64 / 2.0, path.h as f64 / 2.0);
    let to_c = Affine::translate(-cxp, -cyp);
    let from_c = Affine::translate(cxp, cyp);
    let mut cum = vec![Affine::IDENTITY; n];
    for k in 1..n {
        cum[k] = path.steps[k - 1].then_apply(&cum[k - 1]);
    }
    let params: Vec<[f64; 4]> = cum.iter().map(|c| decompose(&to_c.then_apply(c).then_apply(&from_c))).collect();
    // unwrap angles
    let mut params = params;
    for k in 1..n {
        let d = params[k][2] - params[k - 1][2];
        let wrap = (d / std::f64::consts::TAU).round() * std::f64::consts::TAU;
        params[k][2] -= wrap;
    }
    let smoothed: Vec<[f64; 4]> = if no_motion {
        let mean = params.iter().fold([0.0; 4], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2], a[3] + p[3]]).map(|v| v / n as f64);
        vec![mean; n]
    } else {
        // the loop below runs 6 sigma + 1 times per frame: a project-supplied radius must not size it
        // a NaN smoothness falls back to the minimum (`clamp` would pass the NaN on)
        let sigma = if smooth_frames.is_nan() { 0.01 } else { smooth_frames.clamp(0.01, MAX_SMOOTH_SIGMA) };
        let r = (sigma * 3.0).ceil() as isize;
        (0..n as isize)
            .map(|k| {
                let mut acc = [0.0; 4];
                let mut ws = 0.0;
                for j in -r..=r {
                    let i = (k + j).clamp(0, n as isize - 1) as usize;
                    let w = (-(j as f64).powi(2) / (2.0 * sigma * sigma)).exp();
                    for c in 0..4 {
                        acc[c] += params[i][c] * w;
                    }
                    ws += w;
                }
                acc.map(|v| v / ws)
            })
            .collect()
    };
    (0..n)
        .map(|k| {
            let mut sp = smoothed[k];
            if preserve_scale {
                sp[3] = params[k][3];
            }
            // in centred coordinates: smoothed ∘ raw⁻¹
            let raw = compose(params[k]);
            let sm = compose(sp);
            let corr = raw.inverse().map(|ri| sm.then_apply(&ri)).unwrap_or(Affine::IDENTITY);
            from_c.then_apply(&corr).then_apply(&to_c)
        })
        .collect()
}

/// The corrections of one analysed path for one set of Smoothness / Result / Preserve Scale
/// settings, and the zoom that hides the borders of the worst of them (per `max` zoom allowed).
pub(crate) struct Corrected {
    pub(crate) corr: Vec<Affine>,
    /// Keeps the path the corrections came from alive, so its address can't be reused by another.
    _path: Arc<StabilizerPath>,
    worst: Mutex<Vec<(u64, f64)>>,
}

impl Corrected {
    /// The largest [`cover_scale`] of any correction, with at most `max` zoom: what Crop Less
    /// Smooth Motion framing and Auto-scale need, the same for every frame of the clip.
    pub(crate) fn worst_cover(&self, w: f64, h: f64, max: f64) -> f64 {
        let key = max.to_bits();
        if let Some(v) = self.worst.lock().ok().and_then(|l| l.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)) {
            return v;
        }
        let v = self.corr.iter().map(|c| cover_scale(c, w, h, max)).fold(1.0, f64::max);
        if let Ok(mut l) = self.worst.lock() {
            l.push((key, v));
        }
        v
    }
}

type CorrectedKey = (usize, u64, bool, bool);

/// Corrections already worked out, by path and settings.
#[derive(Default)]
pub(crate) struct CorrectedCache(Mutex<HashMap<CorrectedKey, Arc<Corrected>>>);

impl CorrectedCache {
    pub(crate) fn get(&self, path: &Arc<StabilizerPath>, smooth_frames: f64, no_motion: bool, preserve_scale: bool) -> Arc<Corrected> {
        let key = (Arc::as_ptr(path) as usize, smooth_frames.to_bits(), no_motion, preserve_scale);
        if let Some(c) = self.0.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return c;
        }
        let c = Arc::new(Corrected { corr: corrections(path, smooth_frames, no_motion, preserve_scale), _path: path.clone(), worst: Mutex::new(Vec::new()) });
        if let Ok(mut m) = self.0.lock() {
            // a keyframed Smoothness makes a new key for every frame: don't keep them all
            if m.len() >= 16 {
                m.clear();
            }
            m.insert(key, c.clone());
        }
        c
    }
}

/// [`corrections`] for `path`, computed once per set of settings instead of for every rendered
/// frame: smoothing the whole path and finding its worst border took 0.8 ms per frame for a 30 s
/// clip, 7 ms for 2 minutes and 15 ms for 10 minutes, on every frame of playback and export.
pub(crate) fn corrected(path: &Arc<StabilizerPath>, smooth_frames: f64, no_motion: bool, preserve_scale: bool) -> Arc<Corrected> {
    static CACHE: OnceLock<CorrectedCache> = OnceLock::new();
    CACHE.get_or_init(Default::default).get(path, smooth_frames, no_motion, preserve_scale)
}
/// Smallest zoom about the centre (≥ 1) that hides the borders a correction exposes.
pub(crate) fn cover_scale(corr: &Affine, w: f64, h: f64, max: f64) -> f64 {
    let Some(inv) = corr.inverse() else { return 1.0 };
    let (cx, cy) = (w / 2.0, h / 2.0);
    let covered = |z: f64| {
        [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].iter().all(|&(x, y)| {
            let q = inv.apply(Vec2::new(cx + (x - cx) / z, cy + (y - cy) / z));
            q.x >= -0.5 && q.y >= -0.5 && q.x <= w + 0.5 && q.y <= h + 0.5
        })
    };
    if covered(1.0) {
        return 1.0;
    }
    let (mut lo, mut hi) = (1.0, max.max(1.0));
    if !covered(hi) {
        return hi;
    }
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if covered(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Warp Stabilizer: see the module docs.
pub fn warp_stabilizer(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let Some(env) = cx.env else { return };
    let method = if chv(e, "method") == 0 { TrackMethod::Position } else { TrackMethod::PositionScaleRotation };
    let Some(path) = stabilizer_path(env, cx.seconds, method, bv(e, "detailed")) else { return };
    let n = path.steps.len() + 1;
    let k = ((cx.seconds * path.fps).round().max(0.0) as usize).min(n - 1);
    let smooth = fv(e, "smoothness", cx) as f64 / 100.0 * path.fps;
    let crop_less = fv(e, "crop_less", cx) as f64 / 100.0;
    let corrected = corrected(&path, smooth * (0.5 + crop_less), chv(e, "result") == 1, bv(e, "preserve_scale"));
    let corr = &corrected.corr;
    let framing = chv(e, "framing");
    let (aw, ah) = (path.w as f64, path.h as f64);
    let max_scale = (fv(e, "max_scale", cx) as f64 / 100.0).max(1.0);
    let safe = 1.0 + fv(e, "action_safe", cx) as f64 / 100.0;
    let extra = fv(e, "additional_scale", cx) as f64 / 100.0;
    let zoom = match framing {
        2 => corrected.worst_cover(aw, ah, max_scale) * safe,
        _ => 1.0,
    } * extra;
    // analysis px → layer px
    let s = img.w as f64 / aw;
    let to_layer = Affine::scale(s, s);
    let from_layer = Affine::scale(1.0 / s, 1.0 / s);
    let (lw, lh) = (img.w as f64, img.h as f64);
    let z = Affine::translate(lw / 2.0, lh / 2.0).then_apply(&Affine::scale(zoom, zoom)).then_apply(&Affine::translate(-lw / 2.0, -lh / 2.0));
    let m = z.then_apply(&to_layer).then_apply(&corr[k]).then_apply(&from_layer);
    let src = img.clone();
    affine_warp(img, &m);
    match framing {
        1 => {
            // crop to the area every frame covers
            let worst = corrected.worst_cover(aw, ah, 4.0);
            let (mx, my) = (lw * (1.0 - 1.0 / worst) / 2.0, lh * (1.0 - 1.0 / worst) / 2.0);
            crate::effects::crop(img, (mx / lw) as f32, (my / lh) as f32, (mx / lw) as f32, (my / lh) as f32, 0.0);
        }
        3 => {
            // synthesise edges: fill uncovered pixels from the clamped source
            if let Some(inv) = m.inverse() {
                let w = img.w;
                img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let p = &mut row[x * 4..x * 4 + 4];
                        if p[3] < 0.999 {
                            let q = inv.apply(Vec2::new(x as f64 + 0.5, y as f64 + 0.5));
                            let fill = src.sample_bilinear_clamped(q.x as f32, q.y as f32);
                            let mut b = fill;
                            over(&mut b, [p[0], p[1], p[2], p[3]]);
                            p.copy_from_slice(&b);
                        }
                    }
                });
            }
        }
        _ => {}
    }
}

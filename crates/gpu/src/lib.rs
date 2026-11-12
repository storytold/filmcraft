//! The wgpu compositor.
//!
//! Executes a [`FramePlan`](filmcraft_render::plan::FramePlan) on the GPU: every layer's source is
//! uploaded as textures (Y/Cb/Cr planes stay YUV — conversion to linear RGB happens per pixel in
//! the shader), drawn as a transformed quad with premultiplied "over" blending into a Rgba16Float
//! accumulator, then resolved over black into an sRGB `Rgba8UnormSrgb` texture that the UI
//! registers as a native texture. Uploads are cached by pixel-buffer identity, so a paused frame or
//! a still costs nothing.
//!
//! Every blend mode of [`filmcraft_render::Blend`] runs here. Normal and Dissolve (whose per-pixel
//! pattern is the CPU's 64-bit hash, reproduced in WGSL) use fixed-function "over" blending. The
//! other 25 modes need the colour under the layer: before such a layer is drawn, the accumulator
//! region under its quad is copied into a backdrop texture, and the fragment shader composites
//! with the CPU reference's math (`filmcraft_render::blend::composite`: sRGB-encoded straight
//! colour, W3C formulas, back to linear) and writes the result. Only those layers pay the copy.
//!
//! A layer carrying standard effects ([`filmcraft_render::plan::LayerFx`]) goes through the
//! effect stage ([`fx`]) first: its source is drawn into an `Rgba32Float` working image, the
//! effects run as compute passes in WGSL with the CPU reference's math
//! (`filmcraft_render::gpufx::FxOp::apply`), and the result is drawn like any RGBA layer. Layers
//! without effects take the single draw above.
//!
//! Layers that need converting before upload (linear f32 RGBA from CPU-rendered layers, 16-bit
//! YUV such as ProRes) are converted to half floats by [`prepare`], which frame workers run off the
//! UI thread; [`GpuCompositor::composite_prepared`] then only copies bytes into textures.
//!
//! The CPU plan executor (`filmcraft_render::plan::execute_cpu`) is the oracle; tests compare.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rayon::prelude::*;

use filmcraft_color::{Matrix, Range, Transfer};
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_render::Blend;
use filmcraft_render::plan::{DeviceCaps, FramePlan, PlanLayer, PlanStep};

pub mod adapter;
pub mod export_renderer;
pub mod fx;
pub mod lut;
pub mod mask;
mod transition;
pub use export_renderer::ExportRenderer;
pub use lut::GpuLut;
pub use mask::GpuMask;

/// Output texture format: gamma-encoded RGBA8 (what egui expects of native textures); the resolve
/// shader applies the sRGB encoding.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

struct Uploaded {
    gpu: Arc<GpuPlanes>,
    last_used: u64,
    /// The uploaded pixel buffers, kept alive while cached: the cache key is the buffer address,
    /// and a freed buffer's address can be reused by a different frame (stale texture).
    _pixels: PixelData,
}

/// Size and format of a plane texture: what the pool keys on.
type PlaneKey = (u32, u32, wgpu::TextureFormat);

/// Plane textures the pool keeps for reuse (a few frames' worth: a frame has three or four).
const POOL_MAX: usize = 48;

/// Texels uploaded at most by [`GpuCompositor::warm_up`] (eight 4K 10-bit 4:2:2 frames: the
/// staging buffers a play start needs, kept for reuse, and their plane textures in the pool).
const WARM_UP_BYTES: usize = 272 << 20;

/// Texture rows in a staging buffer start at a multiple of this many bytes.
const ROW_ALIGN: u64 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;

/// Staging buffers kept mapped for reuse ([`Uploader::recycle`]): eight frames of three planes
/// (about 270 MB of host memory at 4K 10-bit 4:2:2; a play start uploads ten frames at once).
const STAGING_FREE_MAX: usize = 24;

/// Staging buffers waiting for their map at most: the GPU runs a few passes behind the submits,
/// so the copies of three or four passes (three planes each, at 50p) are still ahead of a
/// buffer's map. A bound as tight as the free list dropped most buffers before they came back,
/// and the uploads created new ones (a quarter of the planes reused a buffer).
const RECYCLING_MAX: usize = 2 * STAGING_FREE_MAX;

/// A recycled staging buffer's map has not completed / completed / failed (`UploaderInner::recycling`).
const MAP_PENDING: u8 = 0;
const MAP_DONE: u8 = 1;
const MAP_FAILED: u8 = 2;

/// Recorded uploads the compositor has not submitted yet beyond which a worker's staging buffers
/// are dropped instead of kept for reuse (a UI thread that stalled for seconds; bounds memory).
const PENDING_MAX: usize = 256;

/// Bytes of recorded uploads one UI pass submits, the ones its composite needs excepted
/// ([`GpuCompositor::flush_uploads`], [`Uploader::take_copies`]): 1.2 frames of 4K 10-bit
/// 4:2:2 (33 MB). Sixty passes a second then carry the 50 frames a second playback needs with a
/// fifth to spare for a backlog: the frames a preroll decodes at once (six to ten, up to 330 MB)
/// reach the GPU over a quarter of a second instead of as a burst. A frame's copies take about
/// half a millisecond of GPU time with the clocks up, but a discrete GPU that idled between
/// plays runs them, and the composite, five to ten times slower until its driver raises the
/// clocks (timestamp queries: the first composites of a play took 10-30 ms, against 0.5 ms in
/// the steady state), so a burst of several frames in front of the first composite took the GPU
/// past the display frame through the first dozen frames of a play. The preroll now wakes the
/// GPU first ([`WAKE_TARGET_MS`]) and flushes without a budget while it does.
const FLUSH_BUDGET: u64 = 40 << 20;

struct UploaderInner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// A 1x1 texture bound where a frame has no plane.
    dummy: wgpu::TextureView,
    /// The device has normalised 16-bit textures ([`optional_features`]): 16-bit planes upload
    /// as they are, as `R16Unorm`, instead of as half floats converted sample by sample.
    norm16: bool,
    pool: Mutex<Vec<(wgpu::Texture, PlaneKey)>>,
    /// Copies recorded by the uploading threads and not yet submitted ([`Uploader::queue_copies`]),
    /// each with the staging buffers it copies from, to map again once it has run.
    pending: Mutex<Pending>,
    /// Staging bytes submitted in the current UI pass, against [`FLUSH_BUDGET`]; the pass's first
    /// flush resets it ([`GpuCompositor::flush_uploads`]).
    pass_bytes: AtomicU64,
    /// Of `pass_bytes`, the bytes the pass's flush took, and the bytes its composites took
    /// because they draw the textures (`seq <= through` in [`Uploader::take_copies`]); for the
    /// pass log.
    pass_flushed: AtomicU64,
    pass_needed: AtomicU64,
    /// Staging buffers being mapped again after their copies ran ([`Uploader::recycle`]), each
    /// with the outcome its map callback reports (`MAP_PENDING`, `MAP_DONE` or `MAP_FAILED`),
    /// and the ones mapped, ready for the next plane of their size. The callback only sets the
    /// flag: it runs inside the device (during a submit, or while the device is destroyed) and
    /// must not drop a GPU resource there.
    recycling: Mutex<Vec<(wgpu::Buffer, Arc<AtomicU8>)>>,
    free: Mutex<Vec<wgpu::Buffer>>,
    /// Plane textures and staging buffers created (not reused), for `perf.stats`.
    created: (AtomicUsize, AtomicUsize),
}

/// The copies of one upload being recorded: the encoder and the staging buffers they read.
struct Upload {
    enc: wgpu::CommandEncoder,
    staging: Vec<wgpu::Buffer>,
}

/// One upload recorded and not yet submitted ([`Uploader::queue_copies`]).
struct PendingUpload {
    /// Its number in recording order; the planes it fills carry it ([`GpuPlanes::upload_seq`]).
    seq: u64,
    /// How soon the frame is wanted (0: now; a prefetch: its frames ahead of the playhead): the
    /// per-pass budget goes to the lowest first ([`Uploader::take_copies`]).
    prio: u32,
    cb: wgpu::CommandBuffer,
    staging: Vec<wgpu::Buffer>,
    /// Staging bytes copied, for the per-pass budget.
    bytes: u64,
}

/// The recorded uploads in order, and the number the next one gets.
struct Pending {
    next_seq: u64,
    entries: Vec<PendingUpload>,
}

/// Uploads frames' planes to textures, from any thread: the frame workers call it from
/// [`prepare`], so a frame reaches the UI thread already on the GPU, where its upload took
/// about 2 ms per plane (and freeing the previous frame's textures 2 ms more) on the thread
/// that has 20 ms per frame to draw everything. Textures are pooled by size and format and
/// go back to the pool when their [`GpuPlanes`] drop; a write into a reused texture is queued
/// after the draws that read it (one queue, in order).
///
/// The uploading thread only records its copies: the compositor's thread submits them, before
/// anything of its own ([`GpuCompositor::flush_uploads`]). It is the device's only submitter on
/// purpose: wgpu refuses to reconfigure the window's surface (every resize, on the UI thread)
/// when another thread submits during its wait for the GPU, and the GPU compositor was switched
/// off for the whole session by one such resize while the workers submitted their uploads
/// themselves.
///
/// The compositor submits the recorded uploads a composite reads before it ([`GpuPlanes::upload_seq`])
/// and the others at a budget per pass ([`FLUSH_BUDGET`]), so a burst of decoded frames does
/// not delay the picture due now.
///
/// Staging buffers are reused: after a submit, the compositor's thread asks the device to map
/// each submitted buffer again ([`Uploader::recycle`]), and the next plane of that size takes it
/// instead of creating one. A buffer per plane per frame (33 MB a frame at 4K 10-bit 4:2:2) had
/// the device's allocator create and destroy its memory blocks through every play start, under
/// a lock the UI thread took while submitting: 55 ms waits there, and the swapchain behind.
#[derive(Clone)]
pub struct Uploader(Arc<UploaderInner>);

/// A plane texture from the pool, returned to it when dropped.
struct PooledTexture {
    tex: Option<wgpu::Texture>,
    key: PlaneKey,
    pool: Arc<UploaderInner>,
}

impl Drop for PooledTexture {
    fn drop(&mut self) {
        if let Some(t) = self.tex.take() {
            let mut pool = self.pool.pool.lock().unwrap_or_else(PoisonError::into_inner);
            // (full: the oldest goes, so textures of a size no longer in use age out of it
            // instead of keeping the textures in use out of it for good)
            if pool.len() >= POOL_MAX {
                pool.remove(0);
            }
            pool.push((t, self.key));
        }
    }
}

/// A frame's planes on the GPU, with what the shader needs to read them.
pub struct GpuPlanes {
    _textures: Vec<PooledTexture>,
    /// Y, Cb, Cr (or the RGBA texture) and the alpha plane (the dummy texture when there is none).
    views: [wgpu::TextureView; 4],
    kind: u32,
    code_scale: f32,
    /// Multiplier from the alpha texture's sample to alpha 0..1; 0 when the frame has no alpha plane.
    alpha_scale: f32,
    chroma: (u32, u32),
    bytes: usize,
    /// The recorded upload that fills the textures (0: none pending, or copied by the
    /// compositor itself): the compositor submits the uploads up to it before drawing them.
    upload_seq: u64,
}

impl Drop for GpuPlanes {
    /// A frame dropped before its upload was submitted (decoded, then evicted: a seek, a fast
    /// scrub) takes the upload with it: nothing can draw these textures any more, and once they
    /// are back in the pool a later upload may fill them, which a copy still pending for them
    /// would then overwrite whenever it was submitted. (This is what lets the compositor submit
    /// the pending uploads in any order.)
    fn drop(&mut self) {
        if self.upload_seq != 0
            && let Some(t) = self._textures.first()
        {
            Uploader(Arc::clone(&t.pool)).cancel(self.upload_seq);
        }
    }
}

impl std::fmt::Debug for GpuPlanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuPlanes").field("kind", &self.kind).field("bytes", &self.bytes).finish()
    }
}

impl Uploader {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self(Arc::new(UploaderInner {
            device: device.clone(),
            queue: queue.clone(),
            dummy: dummy_tex.create_view(&Default::default()),
            norm16: device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            pool: Mutex::new(Vec::new()),
            pending: Mutex::new(Pending { next_seq: 1, entries: Vec::new() }),
            pass_bytes: AtomicU64::new(0),
            pass_flushed: AtomicU64::new(0),
            pass_needed: AtomicU64::new(0),
            recycling: Mutex::new(Vec::new()),
            free: Mutex::new(Vec::new()),
            created: (AtomicUsize::new(0), AtomicUsize::new(0)),
        }))
    }

    /// Whether 16-bit planes upload as 16-bit codes (`R16Unorm`) on this device.
    pub fn norm16(&self) -> bool {
        self.0.norm16
    }

    /// Textures waiting in the pool (tests).
    pub fn pooled(&self) -> usize {
        self.0.pool.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Staging buffers mapped and waiting for reuse.
    pub fn free_staging(&self) -> usize {
        self.collect_mapped();
        self.0.free.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Move the recycled buffers whose map completed to the free list (dropping the ones whose
    /// map failed, and the oldest free ones over [`STAGING_FREE_MAX`]).
    fn collect_mapped(&self) {
        let mut recycling = self.0.recycling.lock().unwrap_or_else(PoisonError::into_inner);
        if recycling.iter().all(|(_, flag)| flag.load(Ordering::Acquire) == MAP_PENDING) {
            return;
        }
        let mut free = self.0.free.lock().unwrap_or_else(PoisonError::into_inner);
        recycling.retain(|(buf, flag)| match flag.load(Ordering::Acquire) {
            MAP_DONE => {
                // (full: the oldest goes, like the texture pool; the warm-up's buffers are of
                // the sequence's size, and a clip of another size must still get its reuse)
                if free.len() >= STAGING_FREE_MAX {
                    free.remove(0);
                }
                free.push(buf.clone());
                false
            }
            MAP_FAILED => false,
            _ => true,
        });
    }

    /// Plane textures and staging buffers created so far (reuses not counted).
    pub fn created(&self) -> (usize, usize) {
        (self.0.created.0.load(Ordering::Relaxed), self.0.created.1.load(Ordering::Relaxed))
    }

    /// A `w` x `h` texture of `format` holding `bytes` (`bpp` bytes per texel, rows packed): from
    /// the pool when one of that size is free, else new. The bytes go into a staging buffer
    /// mapped on this thread (host-visible, `MAP_WRITE`: neither the map nor the unmap goes
    /// through the queue), and `enc` copies them into the texture, so no other thread waits
    /// while they are copied: `Queue::write_texture` copies under the queue's lock, and at 4K50p
    /// the UI thread stalled behind the frame workers' 33 MB frames for milliseconds per pass.
    /// Rows short of `w * bpp` bytes stay zero.
    fn plane(&self, up: &mut Upload, w: u32, h: u32, format: wgpu::TextureFormat, bytes: &[u8], bpp: u32) -> (PooledTexture, wgpu::TextureView) {
        let key = (w, h, format);
        let pooled = {
            let mut pool = self.0.pool.lock().unwrap_or_else(PoisonError::into_inner);
            pool.iter().rposition(|(_, k)| *k == key).map(|i| pool.swap_remove(i).0)
        };
        let t = pooled.unwrap_or_else(|| {
            self.0.created.0.fetch_add(1, Ordering::Relaxed);
            self.0.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("filmcraft-plane"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
        // rows in the buffer start at a multiple of the copy alignment
        let row = u64::from(w) * u64::from(bpp);
        let padded = row.div_ceil(ROW_ALIGN) * ROW_ALIGN;
        let size = padded * u64::from(h);
        if let (true, Ok(padded32), Ok(padded_len), Ok(row_len)) = (size > 0, u32::try_from(padded), usize::try_from(padded), usize::try_from(row)) {
            // a buffer of this size mapped again after its last copy, else a new one, mapped
            let reused = {
                self.collect_mapped();
                let mut free = self.0.free.lock().unwrap_or_else(PoisonError::into_inner);
                free.iter().rposition(|b| b.size() == size).map(|i| free.swap_remove(i))
            };
            let staging = reused.unwrap_or_else(|| {
                self.0.created.1.fetch_add(1, Ordering::Relaxed);
                self.0.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("filmcraft-plane-staging"),
                    size,
                    usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: true,
                })
            });
            let mapped = match staging.slice(..).get_mapped_range_mut() {
                Ok(mut map) => {
                    // (write-only memory: exact ranges, sliced per row when rows are padded)
                    let total = map.len();
                    // a plane shorter than the texture leaves rows unwritten: zero, not what the
                    // buffer's last upload left there (a new buffer is zeroed, a reused one is not)
                    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) < row.saturating_mul(u64::from(h)) {
                        map.slice(..).fill(0);
                    }
                    if padded == row {
                        let n = bytes.len().min(total);
                        if let Some(src) = bytes.get(..n) {
                            map.slice(..n).copy_from_slice(src);
                        }
                    } else {
                        for (r, src) in bytes.chunks(row_len).enumerate() {
                            let Some(start) = r.checked_mul(padded_len).filter(|&s| s < total) else { break };
                            let n = src.len().min(total - start);
                            if let Some(src) = src.get(..n) {
                                map.slice(start..start + n).copy_from_slice(src);
                            }
                        }
                    }
                    true
                }
                Err(e) => {
                    log::warn!("staging buffer not mapped ({e}); uploading through the queue");
                    false
                }
            };
            staging.unmap();
            if !mapped {
                let n = usize::try_from(row * u64::from(h)).unwrap_or(usize::MAX).min(bytes.len());
                self.0.queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    bytes.get(..n).unwrap_or_default(),
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpp), rows_per_image: Some(h) },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
            } else {
                up.enc.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &staging,
                        layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded32), rows_per_image: Some(h) },
                    },
                    wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
                // (kept with the copy: mapped again once it has run, see `Uploader::recycle`)
                up.staging.push(staging);
            }
        }
        let view = t.create_view(&Default::default());
        (PooledTexture { tex: Some(t), key, pool: self.0.clone() }, view)
    }

    fn encoder(&self) -> Upload {
        Upload { enc: self.0.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-upload") }), staging: Vec::new() }
    }

    /// Keep the copies recorded in `up` for the compositor's next submit ([`Uploader::take_copies`]);
    /// this thread submits nothing (see [`Uploader`]). The staging buffers stay with their copy:
    /// a buffer mapped again before its copy was submitted would fail that submit.
    fn queue_copies(&self, up: Upload, prio: u32) -> u64 {
        let bytes = up.staging.iter().map(wgpu::Buffer::size).fold(0u64, u64::saturating_add);
        let mut pending = self.0.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let staging = if pending.entries.len() < PENDING_MAX { up.staging } else { Vec::new() };
        let seq = pending.next_seq;
        pending.next_seq = seq.saturating_add(1);
        pending.entries.push(PendingUpload { seq, prio, cb: up.enc.finish(), staging, bytes });
        seq
    }

    /// Forget the recorded upload `seq` when it is still pending ([`GpuPlanes::drop`]); its
    /// staging buffers, written but never read, are mapped again for the next frame.
    fn cancel(&self, seq: u64) {
        let staging = {
            let mut pending = self.0.pending.lock().unwrap_or_else(PoisonError::into_inner);
            pending.entries.iter().position(|e| e.seq == seq).map(|i| pending.entries.remove(i).staging)
        };
        if let Some(staging) = staging {
            self.recycle(staging);
        }
    }

    /// The recorded copies to submit now, in the order they were recorded, and the staging
    /// buffers they read: the uploads numbered in `needed` (the caller is about to draw their
    /// textures; `None`: every upload), then further ones, the soonest wanted first (`prio`,
    /// then recording order), while the pass's total stays within `budget` bytes (`pass_bytes`,
    /// which the pass's first flush resets). Uploads never fill the same texture while both are
    /// pending ([`GpuPlanes::drop`]), so any order is right. The caller submits the copies
    /// before any command buffer that reads the textures they fill, then hands the buffers to
    /// [`Uploader::recycle`].
    fn take_copies(&self, needed: Option<&[u64]>, budget: u64) -> (Vec<wgpu::CommandBuffer>, Vec<wgpu::Buffer>) {
        let mut pending = self.0.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.0.pass_bytes.load(Ordering::Relaxed);
        let mut taken = before;
        let mut needed_bytes = 0u64;
        let mut pick = vec![false; pending.entries.len()];
        for (i, e) in pending.entries.iter().enumerate() {
            if needed.is_none_or(|n| n.contains(&e.seq)) {
                if let Some(p) = pick.get_mut(i) {
                    *p = true;
                }
                needed_bytes = needed_bytes.saturating_add(e.bytes);
                taken = taken.saturating_add(e.bytes);
            }
        }
        let mut rest: Vec<usize> = (0..pending.entries.len()).filter(|i| pick.get(*i) == Some(&false)).collect();
        rest.sort_by_key(|i| pending.entries.get(*i).map(|e| (e.prio, e.seq)));
        for i in rest {
            let Some(e) = pending.entries.get(i) else { break };
            if taken.saturating_add(e.bytes) > budget {
                break;
            }
            taken = taken.saturating_add(e.bytes);
            if let Some(p) = pick.get_mut(i) {
                *p = true;
            }
        }
        self.0.pass_bytes.store(taken, Ordering::Relaxed);
        if needed.is_some_and(<[u64]>::is_empty) {
            self.0.pass_flushed.fetch_add(taken.saturating_sub(before), Ordering::Relaxed);
        } else {
            self.0.pass_needed.fetch_add(needed_bytes, Ordering::Relaxed);
        }
        let mut cbs = Vec::new();
        let mut staging = Vec::new();
        let mut kept = Vec::with_capacity(pending.entries.len());
        for (e, take) in pending.entries.drain(..).zip(pick) {
            if take {
                cbs.push(e.cb);
                staging.extend(e.staging);
            } else {
                kept.push(e);
            }
        }
        pending.entries = kept;
        (cbs, staging)
    }

    /// Ask the device to map `staging` again, once the copies just submitted from them have run
    /// (the device completes the maps during a later submit, on the submitting thread); each
    /// buffer then waits for the next plane of its size, up to [`STAGING_FREE_MAX`] of them.
    fn recycle(&self, staging: Vec<wgpu::Buffer>) {
        if staging.is_empty() {
            return;
        }
        self.collect_mapped();
        let mut recycling = self.0.recycling.lock().unwrap_or_else(PoisonError::into_inner);
        for buf in staging {
            if recycling.len() >= RECYCLING_MAX {
                break; // (the rest is dropped: enough are on their way back)
            }
            let flag = Arc::new(AtomicU8::new(MAP_PENDING));
            let report = Arc::clone(&flag);
            buf.map_async(wgpu::MapMode::Write, .., move |mapped| {
                report.store(if mapped.is_ok() { MAP_DONE } else { MAP_FAILED }, Ordering::Release);
            });
            recycling.push((buf, flag));
        }
    }

    /// Recorded copies waiting for the compositor's submit.
    pub fn pending_copies(&self) -> usize {
        self.0.pending.lock().unwrap_or_else(PoisonError::into_inner).entries.len()
    }

    /// Staging bytes of the recorded copies not yet submitted.
    pub fn pending_bytes(&self) -> u64 {
        self.0.pending.lock().unwrap_or_else(PoisonError::into_inner).entries.iter().map(|e| e.bytes).fold(0u64, u64::saturating_add)
    }

    /// The current pass's upload bytes: taken by its flush, taken by its composites because
    /// they draw them, taken in all, and still pending.
    pub fn pass_stats(&self) -> [u64; 4] {
        [
            self.0.pass_flushed.load(Ordering::Relaxed),
            self.0.pass_needed.load(Ordering::Relaxed),
            self.0.pass_bytes.load(Ordering::Relaxed),
            self.pending_bytes(),
        ]
    }

    /// One RGBA texture of `format` as a frame's planes.
    fn rgba(&self, enc: &mut Upload, w: u32, h: u32, format: wgpu::TextureFormat, bytes: &[u8]) -> GpuPlanes {
        // (the two RGBA formats uploaded: sRGB bytes, or linear premultiplied half floats)
        let (bpp, kind) = if format == wgpu::TextureFormat::Rgba16Float { (8, 1) } else { (4, 0) };
        let (t, v) = self.plane(enc, w, h, format, bytes, bpp);
        let dummy = || self.0.dummy.clone();
        GpuPlanes {
            _textures: vec![t],
            views: [v, dummy(), dummy(), dummy()],
            kind,
            code_scale: 1.0,
            alpha_scale: 0.0,
            chroma: (w, h),
            bytes: bytes.len(),
            upload_seq: 0,
        }
    }

    /// Linear premultiplied RGBA given as half floats (`w * h * 8` bytes).
    fn rgba_f16(&self, w: u32, h: u32, half: &[u8], prio: u32) -> GpuPlanes {
        let mut enc = self.encoder();
        let mut planes = self.rgba(&mut enc, w, h, wgpu::TextureFormat::Rgba16Float, half);
        planes.upload_seq = self.queue_copies(enc, prio);
        planes
    }

    /// Upload the planes of `f`, using `prep` when it holds its texels converted beforehand;
    /// `prio` says how soon the frame is wanted ([`PendingUpload::prio`]).
    fn frame(&self, f: &VideoFrame, prep: Option<&Prepared>, prio: u32) -> GpuPlanes {
        let mut enc = self.encoder();
        let mut planes = self.frame_in(&mut enc, f, prep);
        planes.upload_seq = self.queue_copies(enc, prio);
        planes
    }

    /// The copies of [`Uploader::frame`], recorded in `enc`.
    fn frame_in(&self, enc: &mut Upload, f: &VideoFrame, prep: Option<&Prepared>) -> GpuPlanes {
        let (w, h) = (f.width, f.height);
        let dummy = || self.0.dummy.clone();
        match &f.data {
            PixelData::Rgba8(d) => self.rgba(enc, w, h, wgpu::TextureFormat::Rgba8UnormSrgb, d),
            PixelData::RgbaF32(d) => {
                let owned;
                let half = match prep.and_then(|p| p.planes.first()) {
                    Some(p) => p,
                    None => {
                        owned = f32_to_f16_bytes(d);
                        &owned
                    }
                };
                self.rgba(enc, w, h, wgpu::TextureFormat::Rgba16Float, half)
            }
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let f8 = wgpu::TextureFormat::R8Unorm;
                let (ty, y) = self.plane(enc, w, h, f8, &planes[0], 1);
                let (tu, u) = self.plane(enc, cw, ch, f8, &planes[1], 1);
                let (tv, v) = self.plane(enc, cw, ch, f8, &planes[2], 1);
                let a = alpha.as_ref().map(|a| self.plane(enc, w, h, f8, a, 1));
                let alpha_scale = if a.is_some() { 1.0 } else { 0.0 };
                let bytes = planes.iter().map(|p| p.len()).sum::<usize>() + alpha.as_ref().map_or(0, |a| a.len());
                let mut textures = vec![ty, tu, tv];
                let a = a.map(|(t, v)| {
                    textures.push(t);
                    v
                });
                GpuPlanes {
                    _textures: textures,
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    kind: 2,
                    code_scale: 255.0,
                    alpha_scale,
                    chroma: (cw, ch),
                    bytes,
                    upload_seq: 0,
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let scale = (1u32 << bits) as f32;
                // Codes as they are in an R16Unorm texture (a sample is code / 65535) on a device
                // that has the format (straight from the frame's planes: a code's little-endian
                // bytes are its bytes in memory), else half floats of code / 2^bits; texels
                // prepared for the other kind of device are converted here instead of being
                // uploaded wrong.
                let owned;
                let (raw16, texels): (bool, Vec<&[u8]>) = match prep {
                    Some(p) if p.planes.len() >= 3 && (!p.raw16 || self.0.norm16) => (p.raw16, p.planes.iter().map(Vec::as_slice).collect()),
                    _ if self.0.norm16 && cfg!(target_endian = "little") => (true, planes.iter().chain(alpha).map(|p| bytemuck::cast_slice(p)).collect()),
                    _ => {
                        owned = Prepared::yuv16(planes, alpha.as_ref(), *bits, self.0.norm16);
                        (owned.raw16, owned.planes.iter().map(Vec::as_slice).collect())
                    }
                };
                let (format, code_scale) = if raw16 { (wgpu::TextureFormat::R16Unorm, 65535.0) } else { (wgpu::TextureFormat::R16Float, scale) };
                let bytes = texels.iter().map(|b| b.len()).sum();
                let mut plane = |i: usize, pw: u32, ph: u32| texels.get(i).map(|b| self.plane(enc, pw, ph, format, b, 2));
                let (Some((ty, y)), Some((tu, u)), Some((tv, v))) = (plane(0, w, h), plane(1, cw, ch), plane(2, cw, ch)) else {
                    // (every branch above yields three planes; a frame without them draws nothing)
                    return GpuPlanes {
                        _textures: Vec::new(),
                        views: [dummy(), dummy(), dummy(), dummy()],
                        kind: 2,
                        code_scale,
                        alpha_scale: 0.0,
                        chroma: (cw, ch),
                        bytes: 0,
                        upload_seq: 0,
                    };
                };
                // (full-resolution alpha plane: its codes span 0..2^bits - 1, hence the scale)
                let a = alpha.as_ref().and_then(|_| plane(3, w, h));
                let alpha_scale = if a.is_some() { code_scale / (scale - 1.0) } else { 0.0 };
                let mut textures = vec![ty, tu, tv];
                let a = a.map(|(t, v)| {
                    textures.push(t);
                    v
                });
                GpuPlanes {
                    _textures: textures,
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    kind: 2,
                    code_scale,
                    alpha_scale,
                    chroma: (cw, ch),
                    bytes,
                    upload_seq: 0,
                }
            }
        }
    }
}

pub struct GpuCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_pipeline: wgpu::RenderPipeline,
    /// Blend modes that read the destination: `fs_blend`, no fixed-function blending.
    blend_pipeline: wgpu::RenderPipeline,
    final_pipeline: wgpu::RenderPipeline,
    layer_bgl: wgpu::BindGroupLayout,
    blend_bgl: wgpu::BindGroupLayout,
    final_bgl: wgpu::BindGroupLayout,
    accum: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    /// Copy of the accumulator under a blend-mode layer (allocated on first use).
    backdrop: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    output: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    accum_readback_buf: Option<(wgpu::Buffer, u64)>,
    uploads: HashMap<(usize, u32, u32), Uploaded>,
    clock: u64,
    /// GPU time of the composites, when the device has timestamp queries.
    timer: Option<GpuTimer>,
    /// [`Self::wake`]: its pipeline, the uniform with the loop length and its bind group, its
    /// target, and the loop length the next one gets.
    wake_pipeline: wgpu::RenderPipeline,
    wake_buf: wgpu::Buffer,
    wake_bind: wgpu::BindGroup,
    wake_target: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    wake_iterations: u32,
    /// The loop length that took [`WAKE_TARGET_MS`] with the clocks up: the longest a preroll
    /// ended with ([`Self::wake_done`]). None before the first preroll of the process.
    wake_ref: Option<u32>,
    /// This preroll's loads have run at the reference length in their time: the clocks are up.
    wake_warm: bool,
    /// Staging bytes a UI pass submits beyond its composites' needs ([`FLUSH_BUDGET`]).
    upload_budget: u64,
    /// The [`GpuPlanes::upload_seq`] of the frames the composite being recorded draws: its
    /// submit carries those uploads first ([`Uploader::take_copies`]); the rest follow at the
    /// per-pass budget, the soonest wanted first.
    needed: Vec<u64>,
    dummy: wgpu::TextureView,
    /// The effect stage (None on devices without compute shaders or storage textures: layers
    /// with effects are then rendered on the CPU here).
    fx: Option<fx::FxStage>,
    transition: transition::PushStage,
    transition_inputs: [Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>; 2],
    pub gpu_transitions: u64,
    pub cpu_transitions: u64,
    /// Adjustment layers ([`PlanStep::Adjust`]) run by the effect stage, and frames with one that
    /// were composited on the CPU here instead: a plan made for another device ([`Self::caps`]), or
    /// the effect stage declining it. Planning with this device's caps keeps the second at zero.
    pub gpu_adjustments: u64,
    pub cpu_adjustments: u64,
    /// Total bytes uploaded (stats).
    pub uploaded_bytes: u64,
    uploader: Uploader,
}

/// The optional device features the compositor uses when `adapter` has them: pass them in the
/// device descriptor's `required_features` (the app, the export renderer and the benches do).
pub fn optional_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & (wgpu::Features::TEXTURE_FORMAT_16BIT_NORM | GpuTimer::FEATURES)
}

/// The GPU time of the compositor's submits, from timestamp queries: a pass that waited for the
/// swapchain can be told apart as a GPU behind (long GPU times) or a late picture. Two queries
/// around each submit, resolved into one of a few small readback buffers; the value arrives a
/// pass or two later ([`GpuTimer::collect`]).
struct GpuTimer {
    qs: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    /// Readback buffers and their state: `TIMER_FREE`, or the map outcome once copied into
    /// (`MAP_PENDING` while the GPU works, then `MAP_DONE` / `MAP_FAILED`); the kind of submit
    /// in flight and what the caller noted about it ([`Self::wrap`]).
    slots: Vec<(wgpu::Buffer, Arc<AtomicU8>, AtomicU8, AtomicU32)>,
    /// Nanoseconds per timestamp tick.
    period: f64,
    /// The latest measurement (ms) of each kind of submit ([`TIMER_COMPOSITE`], [`TIMER_WAKE`]),
    /// with the caller's note on that submit.
    last: Mutex<[(f64, u32); 2]>,
}

/// A timer slot not in use.
const TIMER_FREE: u8 = 3;
/// Readback buffers in flight at most (the GPU runs a few passes behind).
const TIMER_SLOTS: usize = 4;
/// What a timed submit was, for [`GpuTimer::last_ms`]: a composite, or [`GpuCompositor::wake`].
const TIMER_COMPOSITE: u8 = 0;
const TIMER_WAKE: u8 = 1;

/// GPU time one [`GpuCompositor::wake`] aims for, per UI pass: at 60 passes a second half the
/// GPU's time, enough for a driver to count it busy and raise the clocks, and over within a
/// display frame once they are up. (The driver of a discrete GPU lowers its clocks to a
/// fraction, and its bus link to a lower speed, within a second of the GPU going quiet, and
/// raises them a few hundred milliseconds after it gets busy again: pressed while the Program
/// monitor stood still, Play found a GPU on which a frame's copies and composite took 10-30 ms
/// instead of 1-3, and the first eight frames or so were late. The preroll, which waits for the
/// first frames anyway, now puts this load on it meanwhile.)
const WAKE_TARGET_MS: f64 = 8.0;
/// Side of the square target [`GpuCompositor::wake`] draws into (a million fragments: enough to
/// occupy a large GPU; the loop length does the rest).
const WAKE_SIZE: u32 = 1024;
/// Bounds of the per-fragment loop; the compositor sizes it from the time the last one took.
const WAKE_ITERATIONS_MIN: u32 = 64;
const WAKE_ITERATIONS_MAX: u32 = 1 << 18;
/// The first load, before any measurement: a few milliseconds on a mid-range discrete GPU.
const WAKE_ITERATIONS_START: u32 = 4096;

impl GpuTimer {
    const FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(Self::FEATURES) {
            return None;
        }
        let qs = device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("filmcraft-gpu-timer"), ty: wgpu::QueryType::Timestamp, count: 2 });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("filmcraft-gpu-timer-resolve"),
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..TIMER_SLOTS)
            .map(|_| {
                let buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("filmcraft-gpu-timer-read"),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                (buf, Arc::new(AtomicU8::new(TIMER_FREE)), AtomicU8::new(TIMER_COMPOSITE), AtomicU32::new(0))
            })
            .collect();
        Some(Self { qs, resolve, slots, period: f64::from(queue.get_timestamp_period()), last: Mutex::new([(0.0, 0); 2]) })
    }

    /// Read the measurements that arrived and free their slots.
    fn collect(&self) {
        for (buf, flag, tag, note) in &self.slots {
            match flag.load(Ordering::Acquire) {
                MAP_DONE => {
                    if let Ok(view) = buf.slice(..).get_mapped_range() {
                        let stamp = |i: usize| view.get(i * 8..i * 8 + 8).and_then(|b| <[u8; 8]>::try_from(b).ok()).map(u64::from_le_bytes);
                        if let (Some(t0), Some(t1)) = (stamp(0), stamp(1))
                            && t1 >= t0
                            && let Some(slot) = self.last.lock().unwrap_or_else(PoisonError::into_inner).get_mut(usize::from(tag.load(Ordering::Relaxed)))
                        {
                            *slot = ((t1 - t0) as f64 * self.period / 1e6, note.load(Ordering::Relaxed));
                        }
                    }
                    buf.unmap();
                    flag.store(TIMER_FREE, Ordering::Release);
                }
                MAP_FAILED => flag.store(TIMER_FREE, Ordering::Release),
                _ => {}
            }
        }
    }

    /// Put a timestamp before and after `cbs` (to be submitted together), a submit of kind `tag`
    /// that the caller notes `note` about (the wake load's loop length; the measurement comes
    /// back with it, a pass or two later); returns the slot that receives the result, to hand to
    /// [`Self::after_submit`]. None when every slot is in flight.
    fn wrap(&self, device: &wgpu::Device, cbs: &mut Vec<wgpu::CommandBuffer>, tag: u8, note: u32) -> Option<usize> {
        self.collect();
        let i = self.slots.iter().position(|(_, flag, _, _)| flag.load(Ordering::Acquire) == TIMER_FREE)?;
        let (buf, flag, slot_tag, slot_note) = self.slots.get(i)?;
        slot_tag.store(tag, Ordering::Relaxed);
        slot_note.store(note, Ordering::Relaxed);
        let mut begin = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-gpu-timer-begin") });
        begin.write_timestamp(&self.qs, 0);
        let mut end = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-gpu-timer-end") });
        end.write_timestamp(&self.qs, 1);
        end.resolve_query_set(&self.qs, 0..2, &self.resolve, 0);
        end.copy_buffer_to_buffer(&self.resolve, 0, buf, 0, Some(16));
        cbs.insert(0, begin.finish());
        cbs.push(end.finish());
        flag.store(MAP_PENDING, Ordering::Release);
        Some(i)
    }

    /// Ask for slot `i`'s result once the GPU has written it.
    fn after_submit(&self, i: usize) {
        let Some((buf, flag, _, _)) = self.slots.get(i) else { return };
        let report = Arc::clone(flag);
        buf.slice(..).map_async(wgpu::MapMode::Read, move |mapped| {
            report.store(if mapped.is_ok() { MAP_DONE } else { MAP_FAILED }, Ordering::Release);
        });
    }

    /// The latest GPU time measured (ms) of the submits of kind `tag`.
    fn last_ms(&self, tag: u8) -> f64 {
        self.last(tag).0
    }

    /// The latest measurement (ms) of submits of kind `tag`, with the caller's note on it; 0
    /// before the first.
    fn last(&self, tag: u8) -> (f64, u32) {
        self.collect();
        self.last.lock().unwrap_or_else(PoisonError::into_inner).get(usize::from(tag)).copied().unwrap_or((0.0, 0))
    }
}

/// A device descriptor with the defaults plus [`optional_features`].
pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor { required_features: optional_features(adapter), ..Default::default() }
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// f32 → IEEE half (round to nearest even), no dependency needed.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        let round = (m >> 12) & 1;
        return sign | (((m >> 13) + round) as u16);
    }
    let m = mant >> 13;
    let round_bits = mant & 0x1fff;
    let mut h = (sign as u32) | ((e as u32) << 10) | m;
    if round_bits > 0x1000 || (round_bits == 0x1000 && (m & 1) == 1) {
        h += 1;
    }
    h as u16
}

/// IEEE half → f32 (the inverse of [`f32_to_f16`]), for reading `Rgba16Float` textures back.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x03ff) as u32;
    let bits = match exp {
        0 => {
            if mant == 0 {
                sign
            } else {
                // Half subnormal: value = mant × 2⁻²⁴ = (1 + frac) × 2^(b−24) with b the mantissa's
                // highest set bit — renormalise into an f32.
                let b = 31 - mant.leading_zeros();
                let frac = (mant ^ (1 << b)) << (23 - b);
                sign | ((103 + b) << 23) | frac
            }
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        _ => sign | ((exp + 112) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

/// [`f16_to_f32`] with a branch-light path for normal numbers and zeros (the bulk of read-back
/// data); subnormals, infinities and NaNs take the general path. Same result for every input.
#[inline(always)]
pub fn f16_to_f32_fast(h: u16) -> f32 {
    let u = u32::from(h);
    let sign = (u & 0x8000) << 16;
    let abs = u & 0x7fff;
    if (0x0400..0x7c00).contains(&abs) {
        // rebias the exponent (127 - 15 = 112) and widen the mantissa
        f32::from_bits(sign | ((abs + (112 << 10)) << 13))
    } else if abs == 0 {
        f32::from_bits(sign)
    } else {
        f16_to_f32(h)
    }
}

/// A frame made ready off the UI thread: its planes uploaded ([`GpuPlanes`], with an
/// [`Uploader`]), or else its texels converted (one byte vector per plane: half floats, or
/// (`raw16`) the codes of a 16-bit frame as they are, for a device that takes them). 8-bit
/// frames need no conversion and have none.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    planes: Vec<Vec<u8>>,
    raw16: bool,
    gpu: Option<Arc<GpuPlanes>>,
}

impl Prepared {
    /// Bytes held: converted texels and texture memory.
    pub fn bytes(&self) -> usize {
        self.planes.iter().map(Vec::len).sum::<usize>() + self.gpu.as_ref().map_or(0, |g| g.bytes)
    }

    /// Whether the frame is already on the GPU.
    pub fn uploaded(&self) -> bool {
        self.gpu.is_some()
    }

    /// The planes (and alpha) of a 16-bit frame for upload: the codes as bytes when `raw16`,
    /// else half floats of `code / 2^bits`.
    fn yuv16(planes: &[Arc<Vec<u16>>; 3], alpha: Option<&Arc<Vec<u16>>>, bits: u32, raw16: bool) -> Self {
        let convert = |p: &Arc<Vec<u16>>| if raw16 { codes_to_le_bytes(p) } else { codes_to_f16_bytes(p, bits) };
        Self { planes: planes.iter().chain(alpha).map(convert).collect(), raw16, gpu: None }
    }
}

/// [`Prepared`] data for every layer of a plan (index-aligned with its layers), or for the
/// fallback image of a [`FramePlan::Image`].
#[derive(Clone, Debug, Default)]
pub struct PreparedPlan {
    layers: Vec<Option<Prepared>>,
    image: Option<Prepared>,
    children: Vec<PreparedPlan>,
}

impl PreparedPlan {
    pub fn bytes(&self) -> usize {
        self.layers.iter().flatten().chain(&self.image).map(Prepared::bytes).sum::<usize>() + self.children.iter().map(Self::bytes).sum::<usize>()
    }
}

/// Values per parallel chunk when converting.
const CHUNK: usize = 1 << 15;

fn f32_to_f16_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, x) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            o.copy_from_slice(&f32_to_f16(*x).to_le_bytes());
        }
    });
    out
}

/// Half floats of `code / 2^bits` for every 16-bit code (built once per bit depth).
fn code_table(bits: u32) -> &'static [[u8; 2]] {
    static TABLES: [std::sync::OnceLock<Vec<[u8; 2]>>; 17] = [const { std::sync::OnceLock::new() }; 17];
    let bits = bits.min(16);
    TABLES[bits as usize].get_or_init(|| {
        let scale = (1u32 << bits) as f32;
        (0..=u16::MAX as u32).map(|c| f32_to_f16(c as f32 / scale).to_le_bytes()).collect()
    })
}

/// The codes as little-endian bytes (the layout of an `R16Unorm` texture).
fn codes_to_le_bytes(v: &[u16]) -> Vec<u8> {
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, c) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            *o = c.to_le_bytes();
        }
    });
    out
}

/// `code / 2^bits` as half floats, through a per-code table (same values as converting each sample).
fn codes_to_f16_bytes(v: &[u16], bits: u32) -> Vec<u8> {
    let table = code_table(bits);
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, c) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            o.copy_from_slice(&table[*c as usize]);
        }
    });
    out
}

/// Make a frame ready for the compositor: uploaded, with an [`Uploader`] (the compositor's,
/// [`GpuCompositor::uploader`]), else its texels converted (None when it uploads as it is).
pub fn prepare_frame(f: &VideoFrame, up: Option<&Uploader>) -> Option<Prepared> {
    prepare_frame_ahead(f, up, 0)
}

/// [`prepare_frame`] for a frame wanted `prio` frames from now (0: now): the uploader submits
/// the soonest wanted first ([`PendingUpload::prio`]).
pub fn prepare_frame_ahead(f: &VideoFrame, up: Option<&Uploader>, prio: u32) -> Option<Prepared> {
    if let Some(u) = up {
        return Some(Prepared { planes: Vec::new(), raw16: u.0.norm16, gpu: Some(Arc::new(u.frame(f, None, prio))) });
    }
    match &f.data {
        PixelData::RgbaF32(d) => Some(Prepared { planes: vec![f32_to_f16_bytes(d)], raw16: false, gpu: None }),
        PixelData::Yuv16 { planes, bits, alpha, .. } => Some(Prepared::yuv16(planes, alpha.as_ref(), *bits, false)),
        PixelData::Rgba8(_) | PixelData::Yuv8 { .. } => None,
    }
}

/// Make every layer of a plan ready for the compositor ([`prepare_frame`]). Thread-safe: run it
/// on a worker (an uploader queues the uploads; nothing waits for the GPU).
pub fn prepare(plan: &FramePlan, up: Option<&Uploader>) -> PreparedPlan {
    prepare_ahead(plan, up, 0)
}

/// [`prepare`] for a frame wanted `prio` frames from now (0: now; see [`prepare_frame_ahead`]).
pub fn prepare_ahead(plan: &FramePlan, up: Option<&Uploader>, prio: u32) -> PreparedPlan {
    match plan {
        FramePlan::Layers { layers, .. } => prepare_layers(layers, up, prio),
        FramePlan::Image(img) => {
            let half = f32_to_f16_bytes(&img.px);
            let image = match up {
                Some(u) => Prepared { planes: Vec::new(), raw16: false, gpu: Some(Arc::new(u.rgba_f16(img.w as u32, img.h as u32, &half, prio))) },
                None => Prepared { planes: vec![half], raw16: false, gpu: None },
            };
            PreparedPlan { image: Some(image), ..Default::default() }
        }
        FramePlan::Composite { steps, .. } => PreparedPlan {
            children: steps
                .iter()
                .flat_map(|s| match s {
                    PlanStep::Layers(layers) => vec![prepare_layers(layers, up, prio)],
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| prepare_layers(ls, up, prio)).collect(),
                    PlanStep::Adjust { .. } => Vec::new(),
                })
                .collect(),
            ..Default::default()
        },
    }
}

fn prepare_layers(layers: &[PlanLayer], up: Option<&Uploader>, prio: u32) -> PreparedPlan {
    PreparedPlan { layers: layers.iter().map(|l| prepare_frame_ahead(&l.frame, up, prio)).collect(), ..Default::default() }
}

impl GpuCompositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
        });
        let layer_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(5),
            ],
        });
        let blend_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend-layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(4),
                tex_entry(5),
            ],
        });
        let final_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("final"),
            entries: &[wgpu::BindGroupLayoutEntry { binding: 0, ..tex_entry(0) }],
        });
        let pl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("layer"), bind_group_layouts: &[Some(&layer_bgl)], immediate_size: 0 });
        let layer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layer"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUM_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let bpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blend-layer"),
            bind_group_layouts: &[Some(&blend_bgl)],
            immediate_size: 0,
        });
        let blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blend-layer"),
            layout: Some(&bpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blend"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let fpl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("final"), bind_group_layouts: &[Some(&final_bgl)], immediate_size: 0 });
        let final_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("final"),
            layout: Some(&fpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_full"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let wake_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wake"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let wpl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("wake"), bind_group_layouts: &[Some(&wake_bgl)], immediate_size: 0 });
        let wake_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wake"),
            layout: Some(&wpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_wake"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let wake_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("filmcraft-wake"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let wake_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wake"),
            layout: &wake_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wake_buf.as_entire_binding() }],
        });
        let uploader = Uploader::new(device, queue);
        let dummy = uploader.0.dummy.clone();
        // The effect stage's pipelines can still be invalid on a device that passes the limits
        // check (Rgba32Float is not renderable everywhere, e.g. software Vulkan/GL). Validation
        // errors would panic, so they are captured: without the stage, effects render on the CPU.
        let fx = fx::FxStage::supported(device)
            .then(|| {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let stage = fx::FxStage::new(device, &shader, &layer_bgl);
                match poll_now(scope.pop()) {
                    Some(None) => Some(stage),
                    Some(Some(e)) => {
                        log::warn!("GPU effects unavailable on this device, rendering them on the CPU: {e}");
                        None
                    }
                    None => None,
                }
            })
            .flatten();
        Self {
            device: device.clone(),
            queue: queue.clone(),
            layer_pipeline,
            blend_pipeline,
            final_pipeline,
            layer_bgl,
            blend_bgl,
            final_bgl,
            accum: None,
            backdrop: None,
            output: None,
            accum_readback_buf: None,
            uploads: HashMap::new(),
            clock: 0,
            timer: GpuTimer::new(device, queue),
            wake_pipeline,
            wake_buf,
            wake_bind,
            wake_target: None,
            wake_iterations: WAKE_ITERATIONS_START,
            wake_ref: None,
            wake_warm: false,
            upload_budget: FLUSH_BUDGET,
            needed: Vec::new(),
            dummy,
            fx,
            transition: transition::PushStage::new(device),
            transition_inputs: [None, None],
            gpu_transitions: 0,
            cpu_transitions: 0,
            gpu_adjustments: 0,
            cpu_adjustments: 0,
            uploaded_bytes: 0,
            uploader,
        }
    }

    /// The uploader that puts frames on this compositor's device, for the frame workers
    /// ([`prepare`]): a frame they uploaded is drawn without an upload on the UI thread.
    pub fn uploader(&self) -> Uploader {
        self.uploader.clone()
    }

    /// Submit `cb` after the uploads recorded so far ([`Uploader`]): every submit of this
    /// compositor goes through here, so a frame's textures are filled before anything reads
    /// them, and this thread stays the device's only submitter.
    fn submit(&self, cb: wgpu::CommandBuffer) {
        Self::submit_on(&self.uploader, &self.queue, cb, None, self.upload_budget, None);
    }

    /// Set the per-pass upload budget ([`FLUSH_BUDGET`] by default; tests and tuning).
    pub fn set_upload_budget(&mut self, bytes: u64) {
        self.upload_budget = bytes;
    }

    /// [`Self::submit`] for a site that holds a mutable borrow of another field, submitting the
    /// recorded uploads `needed` (`None`: all of them; and more within the per-pass budget)
    /// first, timed by `timer` when given.
    fn submit_on(
        uploader: &Uploader,
        queue: &wgpu::Queue,
        cb: wgpu::CommandBuffer,
        needed: Option<&[u64]>,
        budget: u64,
        timer: Option<(&wgpu::Device, &GpuTimer, u8, u32)>,
    ) {
        let (mut cbs, staging) = uploader.take_copies(needed, budget);
        cbs.push(cb);
        let slot = timer.and_then(|(device, t, tag, note)| t.wrap(device, &mut cbs, tag, note));
        queue.submit(cbs);
        if let (Some((_, t, _, _)), Some(i)) = (timer, slot) {
            t.after_submit(i);
        }
        uploader.recycle(staging);
    }

    /// GPU time (ms) of the latest timed composite submit (its uploads included), or 0 when the
    /// device has no timestamp queries. It lags the pass log by a pass or two.
    pub fn gpu_ms(&self) -> f64 {
        self.timer.as_ref().map_or(0.0, |t| t.last_ms(TIMER_COMPOSITE))
    }

    /// GPU time (ms) of the latest [`Self::wake`] (0 without timestamp queries), and the loop
    /// length the next one gets.
    pub fn wake_stats(&self) -> (f64, u32) {
        (self.timer.as_ref().map_or(0.0, |t| t.last_ms(TIMER_WAKE)), self.wake_iterations)
    }

    /// Whether the GPU's clocks are up, as far as this preroll's [`Self::wake`] loads can tell:
    /// a load of the length that took [`WAKE_TARGET_MS`] at the end of an earlier preroll takes
    /// that again. False until the first measurement of a preroll, and through the first
    /// preroll of the process (no reference yet): the caller holds a play for it only so long.
    pub fn wake_warm(&self) -> bool {
        self.wake_warm
    }

    /// The preroll ended: its last loop length is the reference for the next one, when it is
    /// the longest so far (a preroll that ended with the clocks still low leaves it alone).
    pub fn wake_done(&mut self) {
        self.wake_ref = Some(self.wake_ref.map_or(self.wake_iterations, |r| r.max(self.wake_iterations)));
        self.wake_warm = false;
    }

    /// The preroll was cancelled (Stop before the play began): the next preroll wakes the GPU
    /// itself. Unlike [`Self::wake_done`] the interrupted preroll's loop length is no reference.
    pub fn wake_cancelled(&mut self) {
        self.wake_warm = false;
    }

    /// Give the GPU about [`WAKE_TARGET_MS`] of work, whatever its clocks, so that its driver
    /// raises them: called once per UI pass while a play waits for its first frames (the
    /// preroll), when nothing is due on screen. Each load is sized from the time the last one
    /// took (measured by the timestamp queries; without them the first size stays). Nothing waits
    /// for the GPU; the uploads recorded so far go first, as with every submit.
    pub fn wake(&mut self) {
        match &self.timer {
            Some(t) => {
                // (the measurement is of a load a pass or two ago: `measured` is its length)
                let (ms, measured) = t.last(TIMER_WAKE);
                if ms > 0.0 {
                    // a load near the reference length that ran in its time: the clocks are up
                    if ms <= WAKE_TARGET_MS * 1.3 && self.wake_ref.is_some_and(|r| measured >= r / 4 * 3) {
                        self.wake_warm = true;
                    }
                    // towards the target, at most halving or doubling per step
                    let now = f64::from(self.wake_iterations);
                    let next = (now * WAKE_TARGET_MS / ms).clamp(now / 2.0, now * 2.0);
                    self.wake_iterations = (next as u32).clamp(WAKE_ITERATIONS_MIN, WAKE_ITERATIONS_MAX);
                }
            }
            // (no way to tell: never hold a play for it)
            None => self.wake_warm = true,
        }
        let mut data = [0u8; 16];
        if let Some(d) = data.get_mut(..4) {
            d.copy_from_slice(&self.wake_iterations.to_le_bytes());
        }
        self.queue.write_buffer(&self.wake_buf, 0, &data);
        let view = Self::target(&self.device, &mut self.wake_target, WAKE_SIZE, WAKE_SIZE, OUTPUT_FORMAT, wgpu::TextureUsages::empty());
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-wake") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wake"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.wake_pipeline);
            pass.set_bind_group(0, &self.wake_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        Self::submit_on(
            &self.uploader,
            &self.queue,
            enc.finish(),
            Some(&[]),
            self.upload_budget,
            self.timer.as_ref().map(|t| (&self.device, t, TIMER_WAKE, self.wake_iterations)),
        );
    }

    /// [`Uploader::pass_stats`] of this compositor's uploader.
    pub fn pass_stats(&self) -> [u64; 4] {
        self.uploader.pass_stats()
    }

    /// Submit the uploads the frame workers recorded so far, up to [`FLUSH_BUDGET`] bytes for
    /// this pass (the composites that follow in the pass share the budget), so that the GPU
    /// copies them while the frames are still ahead of the playhead instead of all at once
    /// before the composite that first needs one. Called once per UI pass, first; nothing
    /// happens when there are none.
    pub fn flush_uploads(&self) {
        self.flush_uploads_within(self.upload_budget);
    }

    /// [`Self::flush_uploads`] with a budget of `budget` bytes for the pass this call starts.
    pub fn flush_uploads_within(&self, budget: u64) {
        self.uploader.0.pass_bytes.store(0, Ordering::Relaxed);
        self.uploader.0.pass_flushed.store(0, Ordering::Relaxed);
        self.uploader.0.pass_needed.store(0, Ordering::Relaxed);
        let (cbs, staging) = self.uploader.take_copies(Some(&[]), budget);
        if !cbs.is_empty() {
            self.queue.submit(cbs);
            self.uploader.recycle(staging);
        }
    }

    fn target(
        device: &wgpu::Device,
        slot: &mut Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        extra: wgpu::TextureUsages,
    ) -> wgpu::TextureView {
        if let Some(s) = slot.as_ref().filter(|s| s.2 == (w, h)) {
            return s.1.clone();
        }
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filmcraft-target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | extra,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        *slot = Some((t, v.clone(), (w, h)));
        v
    }

    /// Upload (or reuse) the textures of a frame, using `prep` when it was uploaded or converted
    /// beforehand.
    fn upload(&mut self, f: &VideoFrame, prep: Option<&Prepared>) -> (usize, u32, u32) {
        let id = match &f.data {
            PixelData::Rgba8(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::RgbaF32(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::Yuv8 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
            PixelData::Yuv16 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
        };
        let key = (id, f.width, f.height);
        self.clock += 1;
        if let Some(u) = self.uploads.get_mut(&key) {
            u.last_used = self.clock;
            if u.gpu.upload_seq != 0 && !self.needed.contains(&u.gpu.upload_seq) {
                self.needed.push(u.gpu.upload_seq);
            }
            return key;
        }
        let gpu = match prep.and_then(|p| p.gpu.clone()) {
            Some(g) => g,
            None => Arc::new(self.uploader.frame(f, prep, 0)),
        };
        if gpu.upload_seq != 0 {
            self.needed.push(gpu.upload_seq);
        }
        self.uploaded_bytes += gpu.bytes as u64;
        self.uploads.insert(key, Uploaded { gpu, last_used: self.clock, _pixels: f.data.clone() });
        // keep the most recent uploads (enough for several stacked layers + playback lookahead)
        if self.uploads.len() > 24 {
            let mut v: Vec<(u64, (usize, u32, u32))> = self.uploads.iter().map(|(k, u)| (u.last_used, *k)).collect();
            v.sort_unstable();
            for (_, k) in v.into_iter().take(self.uploads.len() - 24) {
                self.uploads.remove(&k);
            }
        }
        key
    }

    /// The source description of an uploaded frame.
    fn src_info(&self, f: &VideoFrame, key: (usize, u32, u32)) -> Option<SrcInfo> {
        let up = self.uploads.get(&key)?;
        let code_levels = match &f.data {
            PixelData::Yuv8 { .. } => 256.0,
            PixelData::Yuv16 { bits, .. } => (*bits as f32).exp2(),
            _ => 1.0,
        };
        Some(SrcInfo {
            kind: up.gpu.kind,
            code_scale: up.gpu.code_scale,
            code_levels,
            alpha: up.gpu.alpha_scale,
            chroma: up.gpu.chroma,
            size: (f.width, f.height),
            color: f.color,
            input: None,
        })
    }

    /// The uniforms drawing `frame` onto `fx`'s working image: box-decimated, then turned.
    fn fx_source_uniforms(src: &SrcInfo, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> [f32; UNIFORM_FLOATS] {
        let n = fx.decimation.max(1) as f64;
        let m = filmcraft_geom::Affine::scale(1.0 / n, 1.0 / n).then_apply(&filmcraft_render::plan::quarter_turns(
            fx.turns,
            frame.width as f64,
            frame.height as f64,
        ));
        let mut su = Self::uniforms(src, &m, 1.0, Blend::Normal, (fx.size.0.max(1), fx.size.1.max(1)));
        // Effect sources use the working pixel center, avoiding interpolated vertex coordinates.
        su[26] = n as f32;
        su[27] = (fx.turns % 4) as f32;
        su
    }

    fn uniforms(src: &SrcInfo, m: &filmcraft_geom::Affine, opacity: f32, blend: Blend, out: (u32, u32)) -> [f32; UNIFORM_FLOATS] {
        let (kr, kb) = src.color.matrix.kr_kb();
        let bits_scale = src.code_levels / 256.0; // 2^(bits - 8), independent of texture encoding
        let (yo, ys, co, cs) = match (src.color.range, src.kind) {
            (Range::Limited, 2) => (16.0 * bits_scale, 219.0 * bits_scale, 128.0 * bits_scale, 224.0 * bits_scale),
            (Range::Full, 2) => (0.0, src.code_levels - 1.0, src.code_levels / 2.0, src.code_levels - 1.0),
            _ => (0.0, 1.0, 0.0, 1.0),
        };
        let transfer = match src.color.transfer {
            Transfer::Linear => 1.0,
            Transfer::Pq => 2.0,
            Transfer::Hlg => 3.0,
            _ => 0.0,
        };
        // source pixels per output pixel → supersampling taps
        let sx = (m.a * m.a + m.b * m.b).sqrt();
        let sy = (m.c * m.c + m.d * m.d).sqrt();
        let footprint = (1.0 / sx.min(sy).max(1e-6)) as f32;
        let taps = if footprint > 1.25 { footprint.ceil().min(4.0) } else { 1.0 };
        let _ = Matrix::Bt709;
        // colour management (q0..q5), stage by stage as `InputTransform::apply`
        let q = src.input.map_or([0.0; 24], |p| {
            let curve = match p.curve {
                filmcraft_color::GpuCurve::Sdr => 0.0,
                filmcraft_color::GpuCurve::Linear => 1.0,
                filmcraft_color::GpuCurve::Pq => 2.0,
                filmcraft_color::GpuCurve::Hlg => 3.0,
            };
            let (hl, hk) = p.hlg.unwrap_or(([0.0; 3], 0.0));
            let mx = p.matrix.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
            let (sp, dp) = p.tone.unwrap_or((0.0, 0.0));
            let dl = p.desaturate.unwrap_or([0.0; 3]);
            let on = |b: bool| if b { 1.0 } else { 0.0 };
            #[rustfmt::skip]
            let q = [
                1.0, curve, hk, filmcraft_color::REFERENCE_WHITE_NITS as f32,
                hl[0], hl[1], hl[2], on(p.matrix.is_some()),
                mx[0][0], mx[0][1], mx[0][2], on(p.tone.is_some()),
                mx[1][0], mx[1][1], mx[1][2], sp,
                mx[2][0], mx[2][1], mx[2][2], dp,
                dl[0], dl[1], dl[2], on(p.desaturate.is_some()),
            ];
            q
        });
        let base = [
            m.a as f32,
            m.b as f32,
            m.c as f32,
            m.d as f32,
            m.e as f32,
            m.f as f32,
            out.0 as f32,
            out.1 as f32,
            src.size.0 as f32,
            src.size.1 as f32,
            src.chroma.0 as f32,
            src.chroma.1 as f32,
            opacity,
            src.kind as f32,
            taps,
            transfer,
            yo,
            ys,
            co,
            cs,
            kr,
            kb,
            src.code_scale,
            footprint.max(1.0),
            blend.index() as f32,
            src.alpha,
            0.0,
            0.0,
        ];
        let mut u = [0.0; UNIFORM_FLOATS];
        u[..28].copy_from_slice(&base);
        u[28..].copy_from_slice(&q);
        u
    }

    fn uniform_buffer(&self, u: &[f32; UNIFORM_FLOATS]) -> wgpu::Buffer {
        let bytes: Vec<u8> = u.iter().flat_map(|v| v.to_le_bytes()).collect();
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buf, 0, &bytes);
        buf
    }

    /// Draw a few texels of each kind of frame (8-bit and 16-bit YUV, RGBA) at `w` x `h` once, so
    /// the first frame of a sequence does not pay for the render targets and the driver's first
    /// use of the pipelines: the first composites of a process took 40-60 ms, and a 4K50p frame
    /// has 20. Nothing waits for the GPU.
    pub fn warm_up(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let yuv16 = PixelData::Yuv16 {
            planes: [Arc::new(vec![512; 4]), Arc::new(vec![512; 2]), Arc::new(vec![512; 2])],
            chroma: filmcraft_frame::Chroma::C422,
            bits: 10,
            alpha: None,
        };
        let yuv8 = PixelData::Yuv8 {
            planes: [Arc::new(vec![128; 4]), Arc::new(vec![128; 1]), Arc::new(vec![128; 1])],
            chroma: filmcraft_frame::Chroma::C420,
            alpha: None,
        };
        let rgba = PixelData::Rgba8(Arc::new(vec![0; 16]));
        for data in [yuv16, yuv8, rgba] {
            let frame = Arc::new(VideoFrame { width: 2, height: 2, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
            let plan = FramePlan::Layers {
                width: w as usize,
                height: h as usize,
                layers: vec![PlanLayer::new(frame, filmcraft_geom::Affine::IDENTITY, 1.0, Blend::Normal)],
            };
            self.composite(&plan);
        }
        // Full-size 10-bit 4:2:2 frames (the camera formats) through the uploader, on a thread
        // of their own (270 MB of texels), held together and then let go: the plane textures of
        // eight frames are in the pool and their staging buffers come back for reuse before the
        // first frames are due, instead of being created during them (the first play of a 4K
        // sequence had 30 ms composites while that happened). Letting them go cancels their
        // copies (nothing would draw them; [`GpuPlanes::drop`]), so the GPU copies nothing here.
        let (cw, ch) = (w.div_ceil(2), h);
        let (luma, chroma) = (w as usize * h as usize, cw as usize * ch as usize);
        let frames = (WARM_UP_BYTES / ((luma + 2 * chroma) * 2).max(1)).clamp(1, 8);
        let up = self.uploader.clone();
        let warm = move || {
            let held: Vec<GpuPlanes> = (0..frames)
                .map(|_| {
                    let plane = |n: usize| Arc::new(vec![512u16; n]);
                    let data =
                        PixelData::Yuv16 { planes: [plane(luma), plane(chroma), plane(chroma)], chroma: filmcraft_frame::Chroma::C422, bits: 10, alpha: None };
                    let frame = VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() };
                    up.frame(&frame, None, u32::MAX)
                })
                .collect();
            drop(held);
        };
        let spawned = std::thread::Builder::new().name("filmcraft-warm-up".into()).spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(warm)).is_err() {
                log::warn!("the GPU warm-up panicked; the first frames upload as they come");
            }
        });
        if spawned.is_err() {
            log::warn!("no thread for the GPU warm-up; the first frames upload as they come");
        }
    }

    /// Composite a plan; returns the output view (sRGB, over black) and its size.
    pub fn composite(&mut self, plan: &FramePlan) -> (wgpu::TextureView, (u32, u32)) {
        self.composite_prepared(plan, None)
    }

    /// [`composite`](Self::composite) with texel conversions already done by [`prepare`] (the
    /// result is identical; only the upload work on this thread differs).
    pub fn composite_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> (wgpu::TextureView, (u32, u32)) {
        self.render_plan(plan, prep, true, true)
    }

    fn render_plan(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>, clear: bool, resolve: bool) -> (wgpu::TextureView, (u32, u32)) {
        let owned;
        let (w, h, layers): (u32, u32, &[PlanLayer]) = match plan {
            FramePlan::Layers { width, height, layers } => (*width as u32, *height as u32, layers.as_slice()),
            FramePlan::Image(img) => {
                let frame = match prep.and_then(|p| p.image.as_ref()) {
                    // The texels come from `prep`; the frame only carries size and colour.
                    Some(_) => VideoFrame {
                        width: img.w as u32,
                        height: img.h as u32,
                        data: PixelData::RgbaF32(Arc::new(Vec::new())),
                        ..VideoFrame::rgba_f32(1, 1, vec![0.0; 4])
                    },
                    None => VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px.clone()),
                };
                owned =
                    [PlanLayer { frame: Arc::new(frame), matrix: filmcraft_geom::Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None }];
                (img.w as u32, img.h as u32, &owned[..])
            }
            FramePlan::Composite { width, height, steps } => return self.composite_steps(*width, *height, steps, prep, resolve),
        };
        let (w, h) = (w.max(1), h.max(1));
        let accum_view = Self::target(&self.device, &mut self.accum, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC);
        let backdrop_view = if layers.iter().any(|l| l.blend.reads_destination()) {
            Some(Self::target(&self.device, &mut self.backdrop, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_DST))
        } else {
            None
        };
        let out_view = Self::target(&self.device, &mut self.output, w, h, OUTPUT_FORMAT, wgpu::TextureUsages::COPY_SRC);
        // Layers whose effects the GPU stage can't run (no stage on this device, or a working image
        // larger than a texture) are rendered on the CPU here and drawn as plain RGBA layers.
        let max_side = self.device.limits().max_texture_dimension_2d;
        let cpu_fx: Vec<Option<PlanLayer>> = layers
            .iter()
            .map(|l| {
                let lfx = l.fx.as_ref()?;
                if self.fx.is_some() && lfx.size.0.max(lfx.size.1) <= max_side {
                    return None;
                }
                let img = filmcraft_render::plan::effect_image(&l.frame, lfx);
                Some(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px)), l.matrix, l.opacity, l.blend))
            })
            .collect();
        let resolved: Vec<&PlanLayer> = layers.iter().zip(&cpu_fx).map(|(l, c)| c.as_ref().unwrap_or(l)).collect();
        let layers = resolved;
        let keys: Vec<(usize, u32, u32)> = layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let p = prep.and_then(|p| if matches!(plan, FramePlan::Image(_)) { p.image.as_ref() } else { p.layers.get(i).and_then(Option::as_ref) });
                // (texels converted beforehand belong to the plan's own frame)
                let p = if cpu_fx.get(i).is_some_and(Option::is_some) { None } else { p };
                self.upload(&l.frame, p)
            })
            .collect();
        // (bind group, None for a fixed-function layer or Some(region of the accumulator to copy
        // into the backdrop) for a layer that reads the destination; Some(None): off the output)
        let mut bind_groups = Vec::with_capacity(layers.len());
        // the effect stage of each layer with effects (recorded right before its draw)
        let mut jobs: Vec<Option<fx::FxJob>> = Vec::with_capacity(layers.len());
        if let Some(f) = self.fx.as_mut() {
            f.begin_frame();
        }
        for (l, k) in layers.iter().zip(&keys) {
            // (every frame was just uploaded; a missing upload draws transparent texels)
            let src = self.src_info(&l.frame, *k).unwrap_or(SrcInfo {
                kind: 1,
                code_scale: 1.0,
                code_levels: 1.0,
                alpha: 0.0,
                chroma: (1, 1),
                size: (l.frame.width, l.frame.height),
                color: l.frame.color,
                input: None,
            });
            // HDR / wide-gamut Y'CbCr: the planner's input transform, in the shader (#965)
            let src = SrcInfo { input: l.input.as_ref().filter(|_| src.kind == 2).and_then(|t| t.gpu_params()), ..src };
            let views = self.uploads.get(k).map_or_else(|| std::array::from_fn(|_| self.dummy.clone()), |up| up.gpu.views.clone());
            let job = match &l.fx {
                Some(lfx) => {
                    // the source drawn onto the working image (box-decimated by n, as the CPU decodes)
                    let su = Self::fx_source_uniforms(&src, &l.frame, lfx);
                    let sbuf = self.uniform_buffer(&su);
                    let sbg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fx-source"),
                        layout: &self.layer_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
                        ],
                    });
                    match self.fx.as_mut() {
                        Some(f) => f.job(&self.device, &self.queue, lfx, sbg, &self.dummy),
                        None => None,
                    }
                }
                None => None,
            };
            // what the layer draws: the effect result (linear premultiplied RGBA) or the frame
            let (u, tex) = match &job {
                Some(j) => {
                    let s =
                        SrcInfo { kind: 1, code_scale: 1.0, code_levels: 1.0, alpha: 0.0, chroma: l.size(), size: l.size(), color: l.frame.color, input: None };
                    (Self::uniforms(&s, &l.matrix, l.opacity, l.blend, (w, h)), [j.result.clone(), self.dummy.clone(), self.dummy.clone(), self.dummy.clone()])
                }
                None => {
                    // (a layer with effects only gets here if its job failed, which the CPU
                    // fallback above rules out; it is then drawn without them, where the working
                    // image would be)
                    let m = match &l.fx {
                        Some(lfx) => {
                            let n = 1.0 / lfx.decimation.max(1) as f64;
                            l.matrix.then_apply(&filmcraft_geom::Affine::scale(n, n)).then_apply(&filmcraft_render::plan::quarter_turns(
                                lfx.turns,
                                l.frame.width as f64,
                                l.frame.height as f64,
                            ))
                        }
                        None => l.matrix,
                    };
                    (Self::uniforms(&src, &m, l.opacity, l.blend, (w, h)), views)
                }
            };
            jobs.push(job);
            let buf = self.uniform_buffer(&u);
            let mut entries = vec![
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tex[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&tex[3]) },
            ];
            let (layout, region) = match (&backdrop_view, l.blend.reads_destination()) {
                (Some(b), true) => {
                    entries.push(wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(b) });
                    (&self.blend_bgl, Some(quad_bounds(l, w, h)))
                }
                _ => (&self.layer_bgl, None),
            };
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("layer"), layout, entries: &entries });
            bind_groups.push((bg, region));
        }
        if let Some(f) = self.fx.as_mut() {
            f.end_frame();
        }
        let final_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("final"),
            layout: &self.final_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&accum_view) }],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite") });
        // Runs of fixed-function layers draw in one pass. A layer that reads the destination
        // ends it, copies the accumulator under its quad into the backdrop and draws on its own.
        let accum_tex = self.accum.as_ref().map(|a| a.0.clone());
        let backdrop_tex = self.backdrop.as_ref().map(|b| b.0.clone());
        let mut cleared = !clear;
        let mut i = 0;
        while i < bind_groups.len() || !cleared {
            // a layer's effects run right before it is drawn (pooled working textures are reused
            // by the next layer with effects)
            if let (Some(Some(job)), Some(f)) = (jobs.get(i), self.fx.as_ref()) {
                f.record(&mut enc, job);
            }
            if let Some((bg, Some(region))) = bind_groups.get(i) {
                i += 1;
                let (Some(rect), Some(src), Some(dst)) = (region, &accum_tex, &backdrop_tex) else { continue };
                if !cleared {
                    accum_pass(&mut enc, &accum_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    cleared = true;
                }
                let origin = wgpu::Origin3d { x: rect.0, y: rect.1, z: 0 };
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo { texture: src, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::TexelCopyTextureInfo { texture: dst, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: rect.2, height: rect.3, depth_or_array_layers: 1 },
                );
                let mut pass = accum_pass(&mut enc, &accum_view, wgpu::LoadOp::Load);
                pass.set_pipeline(&self.blend_pipeline);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
                continue;
            }
            // a run of fixed-function layers, up to the next one that reads the destination or
            // has effects to run first
            let breaks = |j: &usize| bind_groups.get(*j).is_some_and(|(_, r)| r.is_some()) || jobs.get(*j).is_some_and(Option::is_some);
            let end = (i + 1..bind_groups.len()).find(breaks).unwrap_or(bind_groups.len()).max(i);
            let load = if cleared { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) };
            let mut pass = accum_pass(&mut enc, &accum_view, load);
            cleared = true;
            pass.set_pipeline(&self.layer_pipeline);
            for (bg, _) in bind_groups.get(i..end).unwrap_or_default() {
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
            }
            i = end;
        }
        if resolve {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.final_pipeline);
            pass.set_bind_group(0, &final_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        // (the uploads this frame reads, then the rest within the budget: see `needed`)
        let needed = std::mem::take(&mut self.needed);
        Self::submit_on(
            &self.uploader,
            &self.queue,
            enc.finish(),
            Some(&needed),
            self.upload_budget,
            self.timer.as_ref().map(|t| (&self.device, t, TIMER_COMPOSITE, 0)),
        );
        (if resolve { out_view } else { accum_view }, (w, h))
    }

    fn composite_steps(
        &mut self,
        width: usize,
        height: usize,
        steps: &[PlanStep],
        prep: Option<&PreparedPlan>,
        resolve: bool,
    ) -> (wgpu::TextureView, (u32, u32)) {
        let cpu = |c: &mut Self| {
            let plan = FramePlan::Composite { width, height, steps: steps.to_vec() };
            c.composite_prepared(&FramePlan::Image(filmcraft_render::plan::execute_cpu(&plan)), None)
        };
        if steps.iter().any(|s| matches!(s, PlanStep::Transition { effect, .. } if effect.effect != "push")) {
            self.cpu_transitions += 1;
            return cpu(self);
        }
        // plan_frame makes Adjust steps only for this device's caps, so the monitor and the export
        // never get here; a plan made for another device still gets the right picture
        if steps.iter().any(|s| matches!(s, PlanStep::Adjust { .. })) && !self.caps().adjusts(width, height) {
            self.cpu_adjustments += 1;
            return cpu(self);
        }
        // one frame for the effect stage's pool: the adjustment steps' working textures stay
        // pooled with those of the layers between them
        if let Some(f) = self.fx.as_mut() {
            f.begin_frame();
        }
        let empty = FramePlan::Layers { width, height, layers: Vec::new() };
        let (canvas, dims) = self.render_plan(&empty, None, true, false);
        let mut children = prep.map(|p| p.children.iter());
        // counted once the frame is done on the GPU: a frame that falls back to the CPU half way
        // throws away the steps drawn so far
        let (mut transitions, mut adjustments) = (0u64, 0u64);
        for step in steps {
            match step {
                PlanStep::Layers(layers) => {
                    let p = children.as_mut().and_then(Iterator::next);
                    self.render_plan(&FramePlan::Layers { width, height, layers: layers.clone() }, p, false, false);
                }
                PlanStep::Transition { inputs, effect, progress, .. } => {
                    let mut views = [self.dummy.clone(), self.dummy.clone()];
                    for (index, layers) in inputs.iter().enumerate() {
                        let p = children.as_mut().and_then(Iterator::next);
                        // Isolate each clip; queued draws keep the parent accumulator and blend order.
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        let (view, _) = self.render_plan(&FramePlan::Layers { width, height, layers: layers.clone() }, p, true, false);
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        views[index] = view;
                    }
                    self.transition.draw((&self.device, &self.queue), &canvas, [&views[0], &views[1]], dims, effect, *progress);
                    transitions += 1;
                }
                PlanStep::Adjust { ops } => {
                    if self.adjust(&canvas, dims, ops).is_none() {
                        if let Some(f) = self.fx.as_mut() {
                            f.end_frame();
                        }
                        self.cpu_adjustments += 1;
                        return cpu(self);
                    }
                    adjustments += 1;
                }
            }
        }
        self.gpu_transitions += transitions;
        self.gpu_adjustments += adjustments;
        let out = self.render_plan(&empty, None, false, resolve);
        if let Some(f) = self.fx.as_mut() {
            f.end_frame();
        }
        out
    }

    /// What this device runs, for planning its frames ([`filmcraft_render::plan::plan_frame`]):
    /// adjustment layers up to its texture limit when it has an effect stage.
    pub fn caps(&self) -> DeviceCaps {
        DeviceCaps { adjust_side: if self.fx.is_some() { self.device.limits().max_texture_dimension_2d } else { 0 } }
    }

    /// The adjustment layer `ops` on the accumulator (`canvas`, of size `w`×`h`): it is drawn
    /// into the effect stage's working image as a linear premultiplied RGBA source, the effects
    /// run there, and the result replaces the accumulator. None when the stage can't run them
    /// (nothing is drawn then).
    fn adjust(&mut self, canvas: &wgpu::TextureView, (w, h): (u32, u32), ops: &[filmcraft_render::gpufx::FxOp]) -> Option<()> {
        let rgba = SrcInfo {
            kind: 1,
            code_scale: 1.0,
            code_levels: 1.0,
            alpha: 0.0,
            chroma: (w, h),
            size: (w, h),
            color: filmcraft_color::ColorInfo::SRGB_FULL,
            input: None,
        };
        let identity = filmcraft_geom::Affine::IDENTITY;
        let mut su = Self::uniforms(&rgba, &identity, 1.0, Blend::Normal, (w, h));
        // (the effect source draw's integer decimation: the canvas is the working image)
        su[26] = 1.0;
        let sbuf = self.uniform_buffer(&su);
        let sbg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("adjust-source"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(canvas) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&self.dummy) },
            ],
        });
        let lfx = filmcraft_render::plan::LayerFx { size: (w, h), decimation: 1, turns: 0, ops: ops.to_vec() };
        let job = self.fx.as_mut()?.job(&self.device, &self.queue, &lfx, sbg, &self.dummy)?;
        let buf = self.uniform_buffer(&Self::uniforms(&rgba, &identity, 1.0, Blend::Normal, (w, h)));
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("adjust-result"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&job.result) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&self.dummy) },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-adjust") });
        self.fx.as_ref()?.record(&mut enc, &job);
        {
            // cleared, then drawn "over" nothing: the adjusted picture replaces the canvas
            let mut pass = accum_pass(&mut enc, canvas, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&self.layer_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
        self.submit(enc.finish());
        Some(())
    }

    /// Export rendering: `plan` composited into the linear float accumulator and read back as
    /// premultiplied RGBA f32 (the sRGB resolve is skipped). Layer plans and composites
    /// (`plan_frame` composites only the transitions and adjustment layers the device draws);
    /// None on a failed read-back.
    pub fn render_export_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> Option<(u32, u32, Vec<f32>)> {
        let t = std::time::Instant::now();
        self.render_plan(plan, prep, true, false);
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite-export") });
        let size = self.copy_accumulator(&mut enc)?;
        self.submit(enc.finish());
        export_renderer::timing::add_submit(t.elapsed());
        self.map_accumulator(size)
    }

    /// Run the effect stage of a layer alone: `frame` decoded into its working image and `fx`
    /// applied, read back as linear premultiplied RGBA f32 (what `LayerFx` ops produce on the
    /// CPU from the same frame). None when the working image does not fit in a texture.
    pub fn effect_image(&mut self, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> Option<(u32, u32, Vec<f32>)> {
        let key = self.upload(frame, None);
        let src = self.src_info(frame, key)?;
        let views = self.uploads.get(&key)?.gpu.views.clone();
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let su = Self::fx_source_uniforms(&src, frame, fx);
        let sbuf = self.uniform_buffer(&su);
        let sbg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-source"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
            ],
        });
        let stage = self.fx.as_mut()?;
        stage.begin_frame();
        let Some(job) = stage.job(&self.device, &self.queue, fx, sbg, &self.dummy) else {
            stage.end_frame();
            return None;
        };
        let row = (w * 16).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        stage.record(&mut enc, &job);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &job.result_texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        Self::submit_on(&self.uploader, &self.queue, enc.finish(), None, self.upload_budget, None);
        stage.end_frame();
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait_mapped(&self.device, &rx)?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        let mut line = Vec::with_capacity((w * 16) as usize);
        for y in 0..h {
            let r = data.get((y * row) as usize..(y * row + w * 16) as usize)?;
            copy_mapped(r, &mut line);
            out.extend(line.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        }
        drop(data);
        buf.unmap();
        Some((w, h, out))
    }

    /// Read the **linear float accumulator** back as premultiplied RGBA f32 (export rendering):
    /// the display texture this crate's [`read_output`](Self::read_output) resolves is sRGB 8-bit
    /// and would clip the working-space data HDR exports need. The accumulator must have been
    /// composited first ([`composite`](Self::composite) / [`composite_prepared`](Self::composite_prepared)).
    pub fn read_accumulator(&mut self) -> Option<(u32, u32, Vec<f32>)> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        let size = self.copy_accumulator(&mut enc)?;
        self.submit(enc.finish());
        self.map_accumulator(size)
    }

    /// Record a copy of the accumulator into the staging buffer (grown when too small, reused
    /// otherwise); returns its width, height and row pitch in bytes.
    fn copy_accumulator(&mut self, enc: &mut wgpu::CommandEncoder) -> Option<(u32, u32, u32)> {
        let (_, _, (w, h)) = self.accum.as_ref()?;
        let (w, h) = (*w, *h);
        let row = w.checked_mul(8)?.div_ceil(256).checked_mul(256)?;
        let needed = u64::from(row) * u64::from(h);
        if self.accum_readback_buf.as_ref().is_none_or(|(_, size)| *size < needed) {
            let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("accum-readback"),
                size: needed,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.accum_readback_buf = Some((b, needed));
        }
        let (tex, _, _) = self.accum.as_ref()?;
        let (buf, _) = self.accum_readback_buf.as_ref()?;
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        Some((w, h, row))
    }

    /// Wait for the staging buffer filled by [`copy_accumulator`](Self::copy_accumulator) and
    /// convert its half floats to f32.
    fn map_accumulator(&mut self, dimensions: (u32, u32, u32)) -> Option<(u32, u32, Vec<f32>)> {
        self.map_accumulator_with_wait(dimensions, wait_mapped)
    }

    /// Keep the wait injectable so failure cleanup can be checked without stalling a driver.
    fn map_accumulator_with_wait(
        &mut self,
        (w, h, row): (u32, u32, u32),
        wait: impl FnOnce(&wgpu::Device, &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>) -> Option<()>,
    ) -> Option<(u32, u32, Vec<f32>)> {
        if w == 0 || h == 0 {
            return None;
        }
        let t = std::time::Instant::now();
        // A failed map may already be unmapped; public unmap would report another error.
        // Own the staging buffer for this attempt and cache it again only after success.
        // Otherwise drop it so the next copy creates fresh storage instead of reusing it.
        let (buf, capacity) = self.accum_readback_buf.take()?;
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait(&self.device, &rx)?;
        export_renderer::timing::add_map_wait(t.elapsed());
        let t = std::time::Instant::now();
        let (width, row) = (w as usize, row as usize);
        let mut out = filmcraft_frame::pool::take_f32_overwritten(width.checked_mul(h as usize)?.checked_mul(4)?);
        let data = slice.get_mapped_range().ok()?;
        let mut line = Vec::with_capacity(width * 8);
        // every row must be there: the pooled output holds stale values until written
        let converted = out.chunks_exact_mut(width * 4).enumerate().all(|(y, dst)| {
            let Some(src) = data.get(y * row..y * row + width * 8) else { return false };
            copy_mapped(src, &mut line);
            for (d, s) in dst.iter_mut().zip(line.as_chunks::<2>().0) {
                *d = f16_to_f32_fast(u16::from_le_bytes(*s));
            }
            true
        });
        drop(data);
        if !converted {
            return None;
        }
        buf.unmap();
        self.accum_readback_buf = Some((buf, capacity));
        export_renderer::timing::add_convert(t.elapsed());
        Some((w, h, out))
    }

    /// Read the output back as RGBA8 (tests / screenshots / thumbnails).
    pub fn read_output(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (tex, _, (w, h)) = self.output.as_ref()?;
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(*h) } },
            wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
        );
        self.submit(enc.finish());
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait_mapped(&self.device, &rx)?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..*h {
            out.extend_from_slice(&data[(y * row) as usize..(y * row + w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        Some((*w, *h, out))
    }
}

/// How long a GPU read-back waits, for the poll and then for the callback. Eight seconds is far
/// longer than a healthy map.
const MAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Wait for the last submission and take its `map_async` callback, within a bound.
///
/// Export readback used `PollType::wait_indefinitely`. On radv that can never return after a failed
/// hardware-encode attempt (#1058), so the export never finishes. Giving the poll an eight second
/// timeout is not a bound on its own, though, and that is why an export could still stop for good: a
/// fence that has not signalled inside the timeout is reported to wgpu as `Ok(false)`, and wgpu
/// deliberately ignores it (`Device::maintain`: "we do not care if the wait succeeded or not, and
/// the `Ok(bool)` variant is ignored"), so `poll` returns `Ok(…)` and a GPU that has not finished
/// looks exactly like one that has. The callback then never arrives, and the blocking `recv` that
/// follows parks the export thread forever — no progress, no CPU, no error.
///
/// So the callback is taken with a timeout of our own. Anything late is `None`: the CPU renderer
/// takes the frame, which is what the poll's timeout was always meant to arrange.
fn wait_mapped<T>(device: &wgpu::Device, rx: &std::sync::mpsc::Receiver<Result<(), T>>) -> Option<()> {
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(MAP_TIMEOUT) }).ok()?;
    rx.recv_timeout(MAP_TIMEOUT).ok()?.ok()
}

/// Copy a row of a mapped read-back buffer into `line` (ordinary memory) in one go, before
/// anything reads it sample by sample. Mapped staging memory can be uncached (Mesa's Intel driver
/// maps it so): reading a 1080p half-float frame there two bytes at a time took 650 ms, a bulk copy
/// of it 85 ms; on cached memory the copy of a row that stays in the CPU cache costs next to nothing.
pub(crate) fn copy_mapped(src: &[u8], line: &mut Vec<u8>) {
    line.clear();
    line.extend_from_slice(src);
}

/// Shader source: kind (0 RGBA8 sRGB, 1 linear premultiplied RGBA, 2 YUV), sampling scales, dimensions, color.
struct SrcInfo {
    kind: u32,
    /// Texture sample -> code units: 255 for R8Unorm, 2^bits for R16Float.
    code_scale: f32,
    /// YUV quantization levels: 2^bits; R8Unorm's decoding scale is one less.
    code_levels: f32,
    /// Alpha texture sample → alpha (0: the frame has no alpha plane).
    alpha: f32,
    chroma: (u32, u32),
    size: (u32, u32),
    color: filmcraft_color::ColorInfo,
    /// Colour management beyond the default decode (a Y'CbCr layer's [`PlanLayer::input`]).
    input: Option<filmcraft_color::GpuInputParams>,
}

/// Floats in a layer's uniform block (`U` in `composite.wgsl`: 13 vec4).
const UNIFORM_FLOATS: usize = 52;

/// Poll a future once: wgpu's native error scopes resolve immediately. `None` if it is still
/// pending (then the caller assumes the worst).
fn poll_now<F: std::future::Future>(f: F) -> Option<F::Output> {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
    }
}

/// A render pass drawing into the accumulator.
fn accum_pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("layers"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// The output pixels a layer's quad can touch, as (x, y, width, height) clamped to the `w`×`h`
/// output with a one-pixel margin; None when it touches none. A non-finite matrix covers it all.
fn quad_bounds(l: &PlanLayer, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let m = &l.matrix;
    let (fw, fh) = (l.size().0 as f64, l.size().1 as f64);
    let pts = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)].map(|(x, y)| (m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f));
    if pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return Some((0, 0, w, h));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in pts {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    // in range before the cast (`as` saturates anyway; NaN is excluded above)
    let px = |v: f64, hi: u32| v.max(0.0).min(hi as f64) as u32;
    let (x0, y0) = (px(x0.floor() - 1.0, w), px(y0.floor() - 1.0, h));
    let (x1, y1) = (px(x1.ceil() + 1.0, w), px(y1.ceil() + 1.0, h));
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

#[cfg(test)]
mod blend_tests;
#[cfg(test)]
mod fx_tests;
#[cfg(test)]
mod tests;

//! What the GPU intra encoders share (ProRes: [`crate::ProResGpuEncoder`]): the device they run
//! on, how a step's wgpu errors are caught and counted, the limit checks every buffer passes before
//! it is created, and read-back with a bound on the wait.
//!
//! The encoders are backends of the CPU encoders (`filmcraft_prores::SliceBackend`): a call that
//! fails returns nothing and the CPU encodes the frame. A wgpu error (validation, out of memory), a
//! failed map or a read-back that takes longer than two seconds is a *device error*; the next frame
//! tries again, and a second device error (or a lost device) retires the backend for its life.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How long a read-back may wait for the poll and then for the map callback.
const MAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Device errors after which a backend stays off.
const MAX_DEVICE_ERRORS: u32 = 2;

/// Why a GPU step gave no answer.
#[derive(Debug)]
pub(crate) enum Fail {
    /// A wgpu error, a failed map or a timeout.
    Device(String),
    /// Not for this device, or the shader found the work inconsistent: the CPU does it, and the
    /// backend stays on.
    Decline,
}

/// The largest storage binding (and buffer) a device takes, in bytes.
pub(crate) fn max_binding(limits: &wgpu::Limits) -> u64 {
    limits.max_storage_buffer_binding_size.min(limits.max_buffer_size)
}

/// Whether the buffers of a `w`×`h` frame fit `limits`: its coefficients (`coeff_bytes`) and its
/// largest other buffer (`out_bytes`) within one storage binding and addressable by 32-bit word
/// indices, and the picture within the texture size (the frame may come from, or go to, a texture).
pub(crate) fn fits(limits: &wgpu::Limits, coeff_bytes: u64, out_bytes: u64, w: u32, h: u32) -> bool {
    let max = max_binding(limits);
    let addressable = |b: u64| b <= max && b / 4 <= u64::from(u32::MAX);
    w > 0 && h > 0 && w <= limits.max_texture_dimension_2d && h <= limits.max_texture_dimension_2d && addressable(coeff_bytes) && addressable(out_bytes)
}

/// `words` as little-endian bytes (what storage buffers hold).
pub(crate) fn le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The two error scopes around one encoder step.
pub(crate) struct Scopes {
    oom: wgpu::ErrorScopeGuard,
    validation: wgpu::ErrorScopeGuard,
}

/// A device an encoder runs on, and its record of device errors.
pub(crate) struct Gpu {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    /// Set when the device is lost (devices this module opened; others count their errors).
    lost: Arc<AtomicBool>,
    errors: u32,
    readback: Option<wgpu::Buffer>,
}

impl Gpu {
    pub(crate) fn new(device: wgpu::Device, queue: wgpu::Queue, lost: Arc<AtomicBool>) -> Gpu {
        Gpu { device, queue, lost, errors: 0, readback: None }
    }

    /// Whether this backend is retired (a second device error, or a lost device).
    pub(crate) fn disabled(&self) -> bool {
        self.errors >= MAX_DEVICE_ERRORS || self.lost.load(Ordering::SeqCst)
    }

    /// Catch the wgpu errors of the step that follows (on this thread).
    pub(crate) fn scopes(&self) -> Scopes {
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        Scopes { oom, validation }
    }

    /// The step's answer, unless it or its scopes report a device error (counted) or it declined.
    pub(crate) fn settle<T>(&mut self, what: &str, scopes: Scopes, r: Result<T, Fail>) -> Option<T> {
        let Scopes { oom, validation } = scopes;
        // (scopes pop in reverse order; a scope that has not resolved counts as an error)
        let text = |r: Option<Option<wgpu::Error>>| match r {
            Some(e) => e.map(|e| e.to_string()),
            None => Some("error scope did not resolve".to_string()),
        };
        let v = text(crate::poll_now(validation.pop()));
        let o = text(crate::poll_now(oom.pop()));
        let error = v.or(o);
        match (error, r) {
            (None, Ok(t)) => Some(t),
            (None, Err(Fail::Decline)) => None,
            (Some(e), _) | (None, Err(Fail::Device(e))) => {
                self.errors = self.errors.saturating_add(1);
                if self.disabled() {
                    log::error!("GPU encoding off for this export after a second device error ({what}: {e}); the CPU encodes");
                } else {
                    log::warn!("GPU encoding: device error ({what}: {e}); this frame is encoded on the CPU");
                }
                self.readback = None;
                None
            }
        }
    }

    /// Run `enc` with copies of `(buffer, offset, bytes)` appended (every offset and size a
    /// multiple of 4) and return the copied bytes in order, waiting at most two seconds.
    pub(crate) fn read(&mut self, mut enc: wgpu::CommandEncoder, copies: &[(&wgpu::Buffer, u64, u64)]) -> Result<Vec<u8>, Fail> {
        let total = copies.iter().try_fold(0u64, |a, c| a.checked_add(c.2)).ok_or(Fail::Decline)?;
        if total == 0 || total > self.device.limits().max_buffer_size {
            return Err(Fail::Decline);
        }
        let rb = match &self.readback {
            Some(b) if b.size() >= total => b.clone(),
            _ => {
                let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("intra-enc-readback"),
                    size: total.next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.readback = Some(b.clone());
                b
            }
        };
        let mut at = 0u64;
        for &(buf, offset, size) in copies {
            enc.copy_buffer_to_buffer(buf, offset, &rb, at, size);
            at += size;
        }
        self.queue.submit([enc.finish()]);
        let slice = rb.slice(..total);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(MAP_TIMEOUT) });
        let mapped = waited.map_err(|e| e.to_string()).and_then(|_| match rx.recv_timeout(MAP_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("read-back timed out".into()),
        });
        if let Err(e) = mapped {
            // (a map still pending would fail the next one: start with a new buffer)
            self.readback = None;
            return Err(Fail::Device(e));
        }
        let out = match slice.get_mapped_range() {
            Ok(view) => view.to_vec(),
            Err(e) => {
                rb.unmap();
                return Err(Fail::Device(e.to_string()));
            }
        };
        rb.unmap();
        Ok(out)
    }
}

/// The off-screen device the encoders open for themselves (shared by every encoder of the
/// process, as export renderers share theirs), with its lost flag; `None` without an adapter, or
/// once it is lost.
pub(crate) fn own_device() -> Option<(wgpu::Device, wgpu::Queue, Arc<AtomicBool>)> {
    type Shared = (wgpu::Instance, wgpu::Device, wgpu::Queue, Arc<AtomicBool>);
    static SHARED: OnceLock<Option<Shared>> = OnceLock::new();
    let (_, device, queue, lost) = SHARED
        .get_or_init(|| {
            // a build without a wgpu backend for this platform has no GPU to offer
            if wgpu::Instance::enabled_backend_features().is_empty() {
                return None;
            }
            let instance = crate::adapter::native_instance();
            let adapter = crate::export_renderer::offscreen_adapter(&instance)?;
            // the adapter's own limits: frames up to 8K fit its buffers
            let desc = wgpu::DeviceDescriptor { label: Some("filmcraft-intra-enc"), required_limits: adapter.limits(), ..Default::default() };
            let (device, queue) = pollster::block_on(adapter.request_device(&desc)).ok()?;
            log::info!("GPU encoding on {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
            let lost = Arc::new(AtomicBool::new(false));
            // wgpu's default handler panics; every encoder step runs inside error scopes, so an
            // error that reaches this handler is unexplained: retire the device
            let l = lost.clone();
            device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
                log::error!("GPU encoding device error outside a step: {e}; the CPU encodes");
                l.store(true, Ordering::SeqCst);
            }));
            let l = lost.clone();
            device.set_device_lost_callback(move |reason, msg| {
                log::error!("GPU encoding device lost ({reason:?}): {msg}; the CPU encodes");
                l.store(true, Ordering::SeqCst);
            });
            Some((instance, device, queue, lost))
        })
        .as_ref()?;
    if lost.load(Ordering::SeqCst) {
        return None;
    }
    Some((device.clone(), queue.clone(), lost.clone()))
}

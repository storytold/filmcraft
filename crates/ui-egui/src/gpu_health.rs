//! Health of the window's graphics device: the device-lost callback and the uncaptured-error
//! handler wgpu offers, recorded as a flag the app reads every frame.
//!
//! Without them a lost device (the driver reset, another process took all of the GPU's memory:
//! in a field report other GPU-heavy processes starved the device three times in three
//! minutes) surfaces as a panic inside egui-wgpu's paint path (`update_buffers`: "Failed to create
//! staging buffer for index data" when `Queue::write_buffer_with` returns `None`), which closed
//! the app with the user's work. The app reads [`GpuHealth::lost`] before it draws: a lost device
//! gets an empty frame (nothing to upload, so nothing to panic on) and the host recreates the
//! window ([`crate::FilmcraftApp::take_graphics_restart`]). A panic that still escapes the paint
//! path is caught by the desktop host around eframe's event loop; see `apps/filmcraft/src/main.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Whether the window's graphics device is still usable, shared with wgpu's callbacks.
#[derive(Debug, Default)]
pub struct GpuHealth {
    lost: AtomicBool,
    /// Why, as the first callback reported it.
    reason: Mutex<Option<String>>,
}

impl GpuHealth {
    /// Record that the device is lost (or as good as: out of memory). The first reason is kept.
    pub fn mark_lost(&self, reason: &str) {
        let mut r = self.reason.lock().unwrap_or_else(PoisonError::into_inner);
        if r.is_none() {
            *r = Some(reason.to_string());
        }
        self.lost.store(true, Ordering::SeqCst);
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// The reason the device is lost, if it is.
    pub fn lost(&self) -> Option<String> {
        if !self.is_lost() {
            return None;
        }
        Some(self.reason.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| "graphics device lost".into()))
    }
}

/// Install the callbacks on the window's device. An error that means the device can no longer
/// serve the window (lost, out of memory, an internal driver failure) marks `health` lost; a
/// validation error (a bug in a shader or a draw) only marks `compositor_broken`, so the preview
/// moves to the CPU compositor while the window keeps drawing on the GPU. Every one is logged
/// with its reason: wgpu's default reaction to all of them is to panic.
pub fn guard_device(device: &eframe::wgpu::Device, health: Arc<GpuHealth>, compositor_broken: Arc<AtomicBool>) {
    let on_error = Arc::clone(&health);
    device.on_uncaptured_error(Arc::new(move |e: eframe::wgpu::Error| {
        compositor_broken.store(true, Ordering::Relaxed);
        match &e {
            eframe::wgpu::Error::Validation { .. } => log::error!("GPU error, switching to the CPU compositor: {e}"),
            eframe::wgpu::Error::OutOfMemory { .. } => {
                log::error!("GPU out of memory; the window's graphics device is treated as lost: {e}");
                on_error.mark_lost(&format!("out of GPU memory: {e}"));
            }
            _ => {
                log::error!("GPU internal error; the window's graphics device is treated as lost: {e}");
                on_error.mark_lost(&format!("GPU internal error: {e}"));
            }
        }
    }));
    device.set_device_lost_callback(move |reason, msg| {
        log::error!("graphics device lost ({reason:?}): {msg}");
        health.mark_lost(&format!("device lost ({reason:?}): {msg}"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reason_is_kept() {
        let h = GpuHealth::default();
        assert_eq!(h.lost(), None);
        assert!(!h.is_lost());
        h.mark_lost("out of GPU memory");
        h.mark_lost("device lost (Unknown): later");
        assert!(h.is_lost());
        assert_eq!(h.lost().as_deref(), Some("out of GPU memory"));
    }
}

//! One-shot completion slots for screenshot requests.
//!
//! The request record in `capture.rs` keeps the request id, output path, deadline, settling policy,
//! geometry and the aggregate admission budget. A slot only correlates one capture with one
//! request, checks the retained image against a per-image byte bound, and wakes the owning
//! viewport.
//!
//! [`CaptureSlot::new`] does not issue a screenshot command. Send the returned
//! [`egui::ScreenshotCallback`] with [`egui::ViewportCommand::Screenshot`] when the capture is due.
//! The callback holds only a weak reference to the slot, so dropping or cancelling the request
//! releases its image and makes late results inert.
//!
//! Every retry needs a fresh slot and a fresh callback; a callback is one-shot, and a slot ignores
//! completions after its result was taken, cancelled or dropped. The slot does not track whether a
//! request is still open: the request record keeps that.
//!
//! A completion that publishes its result just before a concurrent [`CaptureSlot::cancel`] or drop
//! can still deliver its single wakeup afterwards. That extra repaint is harmless: if the
//! cancellation won, the image is released and a later [`CaptureSlot::take`] returns `None`; if
//! `take` won first, the caller already owns the result. A completion that observes a cancelled or
//! taken slot stores no image and does not wake. The slot never waits for another thread while
//! holding its lock.
//!
//! [`CaptureSlot`] holds a strong [`egui::Context`]. Keep it in the request record; storing it in
//! `Context`-owned data (`egui::Memory::data` or a plugin) forms a `Context` → slot → `Context`
//! cycle that keeps both alive.

use std::mem::size_of;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use egui::{Color32, ColorImage, Context, ScreenshotCallback, ViewportId};

/// Why a completed screenshot was rejected and released.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// The checked `width * height` pixel count, or its `width * height * size_of::<Color32>()`
    /// byte count, overflowed `usize`.
    DimensionsOverflow { width: usize, height: usize },
    /// The pixel buffer does not match the reported image dimensions, or the image has zero width
    /// or height.
    MalformedImage { width: usize, height: usize, pixels: usize },
    /// The checked image size exceeds the slot's per-image byte bound.
    ImageTooLarge { bytes: usize, max_bytes: usize },
}

impl core::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DimensionsOverflow { width, height } => {
                write!(f, "screenshot dimensions {width}×{height} overflow the checked pixel or byte count")
            }
            Self::MalformedImage { width, height, pixels } => write!(f, "screenshot has {pixels} pixels for its {width}×{height} dimensions"),
            Self::ImageTooLarge { bytes, max_bytes } => write!(f, "screenshot uses {bytes} bytes, over the {max_bytes}-byte per-image limit"),
        }
    }
}

impl std::error::Error for CaptureError {}

enum SlotState {
    Pending,
    Ready(Result<Arc<ColorImage>, CaptureError>),
    Taken,
    Cancelled,
}

struct Shared {
    state: Mutex<SlotState>,
    /// The live request owns the strong context; the callback only upgrades it briefly to wake.
    context: Context,
    viewport: ViewportId,
    max_image_bytes: usize,
}

/// The live request side of a capture. Keep it alive until the result is taken or the request is
/// cancelled; dropping it cancels the completion and releases any retained image.
#[must_use = "the slot must be kept alive to receive and take the screenshot"]
pub struct CaptureSlot {
    shared: Arc<Shared>,
}

impl CaptureSlot {
    /// Create a completion slot and its one-shot callback. `max_image_bytes` bounds the single
    /// retained image only; the request record reserves the aggregate budget before issuing it.
    pub fn new(context: &Context, viewport: ViewportId, max_image_bytes: usize) -> (Self, ScreenshotCallback) {
        let shared = Arc::new(Shared { state: Mutex::new(SlotState::Pending), context: context.clone(), viewport, max_image_bytes });
        let weak = Arc::downgrade(&shared);
        let callback = ScreenshotCallback::new(move |image| complete(&weak, image));
        (Self { shared }, callback)
    }

    /// Take the completed result, at most once. `None` while the capture is pending, after it was
    /// cancelled, and after a previous call took the image or error.
    pub fn take(&self) -> Option<Result<Arc<ColorImage>, CaptureError>> {
        let mut state = lock(&self.shared.state);
        if !matches!(&*state, SlotState::Ready(_)) {
            return None;
        }
        match core::mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Ready(result) => Some(result),
            _ => None,
        }
    }

    /// Cancel the request and release any completed image, after the slot lock is released.
    pub fn cancel(&self) {
        close(&self.shared);
    }
}

impl Drop for CaptureSlot {
    fn drop(&mut self) {
        close(&self.shared);
    }
}

impl core::fmt::Debug for CaptureSlot {
    /// Only the viewport: logging must not hold or dump screenshot contents.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CaptureSlot").field("viewport", &self.shared.viewport).finish_non_exhaustive()
    }
}

/// Lock the slot state, recovering from poisoning instead of panicking. The guard must be
/// released before entering the egui context.
fn lock(state: &Mutex<SlotState>) -> MutexGuard<'_, SlotState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Mark the slot closed and drop any retained image after releasing the lock.
fn close(shared: &Shared) {
    let previous = {
        let mut state = lock(&shared.state);
        core::mem::replace(&mut *state, SlotState::Cancelled)
    };
    drop(previous);
}

/// Validate a completed image, store it, and wake the owning viewport.
///
/// The slot lock is released before calling into the context, so a repaint observer may take or
/// cancel re-entrantly. `request_repaint_of` takes egui's context lock: completing from a repaint
/// observer or a request-repaint callback, which run under it, would deadlock.
fn complete(weak: &Weak<Shared>, image: Arc<ColorImage>) {
    let Some(shared) = weak.upgrade() else {
        // The request was dropped; the image is released here without a reply.
        return;
    };
    let result = checked_image(image, shared.max_image_bytes);
    let should_wake = {
        let mut state = lock(&shared.state);
        if matches!(&*state, SlotState::Pending) {
            *state = SlotState::Ready(result);
            true
        } else {
            // Cancelled, already taken, or completed twice: drop the image.
            false
        }
    };
    if should_wake {
        shared.context.request_repaint_of(shared.viewport);
    }
}

/// Validate `image` against the per-image byte bound: zero width or height is malformed before any
/// arithmetic, the pixel and byte counts use checked multiplication, the buffer length must equal
/// `width * height`, and a byte count over `max_image_bytes` is rejected (equality is accepted).
fn checked_image(image: Arc<ColorImage>, max_image_bytes: usize) -> Result<Arc<ColorImage>, CaptureError> {
    let [width, height] = image.size;
    if width == 0 || height == 0 {
        return Err(CaptureError::MalformedImage { width, height, pixels: image.pixels.len() });
    }
    let pixels = width.checked_mul(height).ok_or(CaptureError::DimensionsOverflow { width, height })?;
    let bytes = pixels.checked_mul(size_of::<Color32>()).ok_or(CaptureError::DimensionsOverflow { width, height })?;
    if image.pixels.len() != pixels {
        return Err(CaptureError::MalformedImage { width, height, pixels: image.pixels.len() });
    }
    if bytes > max_image_bytes {
        return Err(CaptureError::ImageTooLarge { bytes, max_bytes: max_image_bytes });
    }
    Ok(image)
}

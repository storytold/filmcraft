//! Sampling a colour from anywhere on the screen, the way the eyedropper in an image editor does.
//!
//! The operating system shows its own magnifier over the screen, so the colour can come from this
//! window, from another window, or from anywhere else on the display. Only macOS has this; other
//! systems return nothing, and the eyedropper keeps to the Program picture.

use std::sync::mpsc::{Receiver, TryRecvError};

/// One sampling session: the system magnifier is up until the user clicks or presses Escape.
pub struct ScreenSampler {
    outcome: Receiver<Option<[u8; 3]>>,
    // The session retains the sampler and the handler until it ends; holding them here keeps the
    // callback alive for as long as anyone is polling for its result.
    #[cfg(target_os = "macos")]
    _session: macos::Session,
}

impl ScreenSampler {
    /// `None` while the user is still choosing, `Some(None)` when they cancelled, `Some(Some(rgb))`
    /// once a colour has been picked.
    pub fn poll(&self) -> Option<Option<[u8; 3]>> {
        match self.outcome.try_recv() {
            Ok(v) => Some(v),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

/// Puts the system colour magnifier on screen. `None` when this system cannot sample the screen.
#[cfg(target_os = "macos")]
pub fn start() -> Option<ScreenSampler> {
    macos::start()
}

/// Puts the system colour magnifier on screen. `None` when this system cannot sample the screen.
#[cfg(not(target_os = "macos"))]
pub fn start() -> Option<ScreenSampler> {
    None
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use super::ScreenSampler;
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSColor, NSColorSampler, NSColorSpace};
    use std::sync::mpsc::channel;

    pub(super) struct Session {
        // Both are `Retained`/`RcBlock` values that the sampling session itself also retains, so
        // dropping them early would be safe, but keeping them makes that independent of Apple's
        // wording.
        _sampler: Retained<NSColorSampler>,
        _handler: RcBlock<dyn Fn(*mut NSColor)>,
    }

    /// The sRGB equivalent of a sampled colour, rounded to bytes.
    fn srgb(colour: &NSColor) -> Option<[u8; 3]> {
        let space = NSColorSpace::sRGBColorSpace();
        let colour = colour.colorUsingColorSpace(&space)?;
        let (mut red, mut green, mut blue, mut alpha) = (0.0, 0.0, 0.0, 0.0);
        // SAFETY: the colour is in an RGB space (sRGB) after the conversion above, which is what
        // `getRed:green:blue:alpha:` requires; the four pointers are valid for writes.
        unsafe { colour.getRed_green_blue_alpha(&mut red, &mut green, &mut blue, &mut alpha) };
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some([byte(red), byte(green), byte(blue)])
    }

    pub(super) fn start() -> Option<ScreenSampler> {
        let (sender, outcome) = channel();
        let handler = RcBlock::new(move |colour: *mut NSColor| {
            // SAFETY: the handler is called on the main thread with either a valid colour or a null
            // pointer (cancellation), which `as_ref` turns into `None`.
            let picked = unsafe { colour.as_ref() }.and_then(srgb);
            let _ = sender.send(picked);
        });
        let sampler = NSColorSampler::new();
        // SAFETY: the sampler is a fresh instance, the handler is a live block, and AppKit calls it
        // back on the main thread, which is where this runs (the event loop).
        unsafe { sampler.showSamplerWithSelectionHandler(&handler) };
        Some(ScreenSampler { outcome, _session: Session { _sampler: sampler, _handler: handler } })
    }
}

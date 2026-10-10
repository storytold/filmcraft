//! Documents the operating system asks the app to open (macOS: double-clicking a `.fcproj` in
//! Finder, Open With, dropping it on the Dock icon).
//!
//! On macOS these do not arrive as command-line arguments: Launch Services sends the running app
//! an "open documents" Apple Event, on a cold launch as well as when the app is already open.
//! winit 0.30 does not surface that event, so [`install`] registers a handler for it
//! ([`open_documents_macos`](crate::open_documents_macos), the module's only `unsafe`). The handler
//! queues the paths here and calls the waker; the app drains the queue with [`take`] on its next
//! frame and opens them like command-line files. Paths that arrive before the app has a window
//! (the cold-launch case) simply wait in the queue.
//!
//! Elsewhere [`install`] does nothing: Windows and Linux file associations start the app with the
//! file as an argument.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

/// At most this many queued paths (a runaway sender cannot grow the queue without bound).
const MAX_QUEUED: usize = 1024;

static QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
type Waker = Box<dyn Fn() + Send>;
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// Start receiving open-document requests from the OS. Call once, on the main thread, before the
/// event loop runs (so documents that launched the app are not missed). Returns whether a handler
/// was installed (macOS only); an error says why it could not be.
pub fn install() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        crate::open_documents_macos::install().map(|()| true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

/// Called (on the UI thread or any other) whenever paths are queued, e.g. to request a repaint.
pub fn set_waker(waker: impl Fn() + Send + 'static) {
    *WAKER.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(waker));
    // documents that came in before the waker existed (cold launch) still need a frame
    if !QUEUE.lock().unwrap_or_else(PoisonError::into_inner).is_empty() {
        wake();
    }
}

/// The paths received since the last call, oldest first.
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Queue paths from the OS and wake the app.
pub(crate) fn push(paths: impl IntoIterator<Item = PathBuf>) {
    {
        let mut q = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        for p in paths {
            if q.len() >= MAX_QUEUED {
                log::warn!("open documents: queue full, dropping {}", p.display());
                continue;
            }
            q.push(p);
        }
    }
    wake();
}

fn wake() {
    if let Some(w) = WAKER.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        w();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // One test: the queue and waker are process-wide statics.
    #[test]
    fn queues_paths_wakes_and_caps() {
        let _ = take();
        // a cold launch: paths before the waker; setting it wakes once for them
        push([PathBuf::from("/a.fcproj")]);
        let woke = Arc::new(AtomicUsize::new(0));
        let w = woke.clone();
        set_waker(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(woke.load(Ordering::SeqCst), 1);
        push([PathBuf::from("/b.fcproj"), PathBuf::from("/c.mov")]);
        assert_eq!(woke.load(Ordering::SeqCst), 2);
        assert_eq!(take(), [PathBuf::from("/a.fcproj"), PathBuf::from("/b.fcproj"), PathBuf::from("/c.mov")]);
        assert!(take().is_empty());
        // capped
        push((0..MAX_QUEUED + 10).map(|i| PathBuf::from(format!("/{i}"))));
        assert_eq!(take().len(), MAX_QUEUED);
        *WAKER.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

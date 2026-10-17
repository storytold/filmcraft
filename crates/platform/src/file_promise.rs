//! Files that a macOS drag only *promises* instead of handing over (`NSFilePromiseReceiver`).
//!
//! Dragging a video out of Photos.app offers the movie only as a *file promise*
//! (`com.apple.pasteboard.promised-file-content-type` = `com.apple.quicktime-movie`,
//! `promised-suggested-file-name` = `IMG_5691.MOV`). The only real path on that pasteboard is a
//! JPEG poster frame inside the Photos library
//! (`…/resources/derivatives/D/D3247EC6-….jpeg`), and winit asks the pasteboard for
//! `NSFilenamesPboardType` only, so a dragged-in video arrived as a still image. The
//! movie often has no local file at all (an iCloud asset is downloaded while the promise is
//! fulfilled), so no amount of path rewriting can stand in for asking for the promise.
//!
//! Promises are a drag-time contract: the receiver must be read from the dragging pasteboard
//! *during* the drop, and the files arrive asynchronously afterwards. So this intercepts the two
//! dragging-destination methods on winit's window delegate, takes the drop when the pasteboard
//! carries promises, and reports each file as it lands. Everything else (Finder, a file manager
//! on any other platform) keeps going through winit untouched.
//!
//! Like [`crate::cursor`], this is OS glue rather than media: it lives here because
//! `crates/platform` is the only crate allowed `unsafe` (AGENTS.md §0.3, ADR 0001). The public
//! API is safe, reports failure instead of panicking, and on every other platform [`install`]
//! does nothing and drops stay exactly as winit delivers them.

use std::path::PathBuf;

/// What a host does with promised files: where to put them, and what to do once they land.
pub struct Handlers {
    /// The directory to write received files to, asked for at each drop (so it can follow the
    /// open project's scratch disk). Created if it does not exist.
    pub destination: Box<dyn Fn() -> PathBuf>,
    /// Files that have finished arriving, one call per file, on the main thread. The host imports
    /// them as if they had been dropped, and must ask for a repaint: these arrive between frames.
    pub received: Box<dyn Fn(Vec<PathBuf>)>,
}

/// Takes over promised-file drops for the application's windows. Returns whether it did: `false`
/// off macOS, when not on the main thread, or when the windowing layer's drag handling is not
/// where this expects it (a winit the interception no longer fits) — the caller then keeps
/// winit's plain-path drops, which is what happened before this existed.
#[cfg(not(target_os = "macos"))]
pub fn install(_handlers: Handlers) -> bool {
    false
}

#[cfg(target_os = "macos")]
pub use mac::install;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mac {
    use super::Handlers;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{ClassType, MainThreadMarker, msg_send, sel};
    use objc2_app_kit::{NSApplication, NSFilePromiseReceiver, NSPasteboard, NSPasteboardType};
    use objc2_foundation::{NSArray, NSDictionary, NSError, NSOperationQueue, NSString, NSURL};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::OnceLock;

    thread_local! {
        /// Set by [`install`]; read on the main thread when a drop lands.
        static HANDLERS: RefCell<Option<Handlers>> = const { RefCell::new(None) };
    }

    /// winit's `performDragOperation:` and `draggingEntered:`, kept to handle every drag that
    /// promises nothing.
    static ORIGINAL_PERFORM: OnceLock<Imp> = OnceLock::new();
    static ORIGINAL_ENTERED: OnceLock<Imp> = OnceLock::new();

    /// A dragging-destination method: `(self, _cmd, sender) -> BOOL`. winit declares
    /// `draggingEntered:` with a `BOOL` return too, where AppKit reads an `NSDragOperation`; YES
    /// is `NSDragOperationCopy`, NO is `NSDragOperationNone`, so both keep that signature.
    type DragFn = unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;

    pub fn install(handlers: Handlers) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let app = NSApplication::sharedApplication(mtm);

        // Accept the promise types as well, so a drag that offers nothing but promises (and
        // therefore no `NSFilenamesPboardType`) is not rejected before it can be read.
        let mut types: Vec<Retained<NSPasteboardType>> = NSFilePromiseReceiver::readableDraggedTypes().iter().collect();
        types.push(NSString::from_str("NSFilenamesPboardType"));
        types.push(NSString::from_str("public.file-url"));
        let types = NSArray::from_retained_slice(&types);

        let mut delegate_class: Option<&AnyClass> = None;
        for window in app.windows().iter() {
            window.registerForDraggedTypes(&types);
            if let Some(delegate) = window.delegate() {
                let object: &AnyObject = delegate.as_ref();
                delegate_class = Some(object.class());
            }
        }
        let Some(class) = delegate_class else { return false };

        // One class for every window, so swap the implementations once.
        if ORIGINAL_PERFORM.get().is_none() {
            let Some(perform) = class.instance_method(sel!(performDragOperation:)) else { return false };
            let Some(entered) = class.instance_method(sel!(draggingEntered:)) else { return false };
            let ours_perform: DragFn = perform_drag_operation;
            let ours_entered: DragFn = dragging_entered;
            // SAFETY: both replacements have the `(self, _cmd, id) -> BOOL` signature the runtime
            // and AppKit expect of these two methods, take the same arguments (a possibly-null
            // sender is handled), and fall back to the implementation they replaced for every
            // drag they do not take. `set_implementation` returns that one.
            unsafe {
                let previous_perform = perform.set_implementation(std::mem::transmute::<DragFn, Imp>(ours_perform));
                let previous_entered = entered.set_implementation(std::mem::transmute::<DragFn, Imp>(ours_entered));
                let _ = ORIGINAL_PERFORM.set(previous_perform);
                let _ = ORIGINAL_ENTERED.set(previous_entered);
            }
        }
        HANDLERS.with_borrow_mut(|h| *h = Some(handlers));
        true
    }

    /// Calls the implementation `install` replaced, or reports the drag unhandled when there is
    /// none to call (it was never recorded, so nothing can be forwarded).
    fn forward(original: &OnceLock<Imp>, this: *mut AnyObject, sel: Sel, sender: *mut AnyObject) -> Bool {
        let Some(previous) = original.get() else { return Bool::NO };
        // SAFETY: `previous` is the implementation of the method being forwarded, as returned by
        // `set_implementation`, so it has this signature and takes these arguments.
        unsafe {
            let previous: DragFn = std::mem::transmute::<Imp, DragFn>(*previous);
            previous(this, sel, sender)
        }
    }

    /// Accepts a promise-only drag (winit's answer looks at the file paths, of which such a drag
    /// has none) and otherwise leaves the hover to winit, which reports the hovered files.
    ///
    /// This only asks whether the pasteboard *offers* promises. Reading the receivers themselves
    /// here would answer the same question and break the drop: each promise can be claimed once,
    /// and the receivers this hover created are gone by the time the file is dropped, leaving
    /// `performDragOperation:` with nothing to ask (dragged videos imported as their poster frame
    /// again, which is the bug this module exists to fix).
    unsafe extern "C" fn dragging_entered(this: *mut AnyObject, sel: Sel, sender: *mut AnyObject) -> Bool {
        let hovered = forward(&ORIGINAL_ENTERED, this, sel, sender);
        if hovered.as_bool() {
            return hovered;
        }
        let promised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pasteboard(sender).is_some_and(|pb| promises_files(&pb))));
        Bool::new(promised.unwrap_or(false))
    }

    /// Whether a pasteboard offers files as promises, without claiming them.
    fn promises_files(pb: &NSPasteboard) -> bool {
        pb.availableTypeFromArray(&NSFilePromiseReceiver::readableDraggedTypes()).is_some()
    }

    /// Takes a drop whose files are promised, and leaves every other drop to winit.
    ///
    /// The promises are claimed here, which only this moment allows, but asking for the files
    /// waits for the next turn of the run loop: `receivePromisedFilesAtDestination:…` runs the
    /// run loop itself, and winit panics when an event reaches it while it is still handling this
    /// drop ("tried to handle event while another event is currently being handled"), taking the
    /// drag down with it.
    unsafe extern "C" fn perform_drag_operation(this: *mut AnyObject, sel: Sel, sender: *mut AnyObject) -> Bool {
        let taken = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let pb = pasteboard(sender)?;
            let receivers = receivers(&pb);
            if receivers.is_empty() {
                return None;
            }
            // The plain paths stand in for promises that fail, so they are read while the
            // dragging pasteboard still exists.
            Some(after_this_event(this, receivers, plain_paths(&pb)))
        }));
        match taken {
            Ok(Some(true)) => Bool::YES,
            // Nothing promised, or the files could not be asked for: the plain paths (a poster
            // frame, for Photos.app, but better than losing the drop) are winit's to deliver.
            Ok(Some(false)) | Ok(None) | Err(_) => forward(&ORIGINAL_PERFORM, this, sel, sender),
        }
    }

    /// Queues the fulfilment of `receivers` for the next turn of the run loop, once winit has
    /// finished handling the drop. Returns whether it could be queued.
    fn after_this_event(delegate: *mut AnyObject, receivers: Vec<Retained<NSFilePromiseReceiver>>, fallback: Vec<PathBuf>) -> bool {
        if MainThreadMarker::new().is_none() {
            return false;
        }
        let Some(delegate) = std::ptr::NonNull::new(delegate) else { return false };
        // SAFETY: `delegate` is the window delegate AppKit sent `performDragOperation:` to, so it
        // is a live object, retained here for as long as the queued block needs it.
        let delegate = unsafe { Retained::retain(delegate.as_ptr()) };
        let Some(delegate) = delegate else { return false };
        let once = RefCell::new(Some((receivers, fallback, delegate)));
        let block = block2::RcBlock::new(move || {
            let Some((receivers, fallback, delegate)) = once.borrow_mut().take() else { return };
            // winit clears the files it reported as hovering when it sees the drop or the drag
            // leaving. It sees neither of a drop taken here, and the UI would go on believing
            // files are over the window, so tell it the drag is gone.
            let delegate: &AnyObject = &delegate;
            // SAFETY: the delegate is retained above and implements `draggingExited:`, whose
            // sender may be nil.
            unsafe { msg_send![delegate, draggingExited: std::ptr::null_mut::<AnyObject>()] }
            receive(&receivers, fallback);
        });
        // SAFETY: the block takes no arguments and returns nothing, as the queue calls it, and
        // it is run on the main thread (the main queue's only thread), where everything it
        // touches belongs.
        unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
        true
    }

    /// The dragging pasteboard of a `NSDraggingInfo` sender.
    fn pasteboard(sender: *mut AnyObject) -> Option<Retained<NSPasteboard>> {
        let sender = std::ptr::NonNull::new(sender)?;
        // SAFETY: AppKit passes a non-null `NSDraggingInfo` to these methods, whose
        // `draggingPasteboard` is a non-null `NSPasteboard` it owns.
        unsafe { msg_send![sender.as_ptr(), draggingPasteboard] }
    }

    /// The file promises on a pasteboard, in the order the dragging source offered them.
    fn receivers(pb: &NSPasteboard) -> Vec<Retained<NSFilePromiseReceiver>> {
        let classes = NSArray::from_slice(&[NSFilePromiseReceiver::class()]);
        // SAFETY: the class array holds exactly the class the results are downcast to, and no
        // read options are passed.
        let read = unsafe { pb.readObjectsForClasses_options(&classes, None) };
        read.map(|objects| objects.iter().filter_map(|o| o.downcast::<NSFilePromiseReceiver>().ok()).collect()).unwrap_or_default()
    }

    /// Asks the dragging source to write every promised file into the host's destination
    /// directory, and reports each one as it lands. The plain paths of the drag stand in when
    /// every promise fails, so an unusable destination or a source that gives up does not leave
    /// the drop with nothing.
    fn receive(receivers: &[Retained<NSFilePromiseReceiver>], fallback: Vec<PathBuf>) {
        let Some(destination) = HANDLERS.with_borrow(|h| h.as_ref().map(|h| (h.destination)())) else { return };
        if let Err(e) = std::fs::create_dir_all(&destination) {
            log::warn!("dropped promised files: {} is not writable: {e}", destination.display());
            deliver(fallback);
            return;
        }
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&destination.to_string_lossy()), true);

        let state = Rc::new(RefCell::new(Pending { outstanding: receivers.len(), delivered: false, fallback }));
        let queue = NSOperationQueue::mainQueue();
        let options = NSDictionary::new();
        for receiver in receivers {
            let state = Rc::clone(&state);
            let reader = block2::RcBlock::new(move |url: std::ptr::NonNull<NSURL>, error: *mut NSError| {
                // SAFETY: AppKit passes a non-null `NSURL` for a file it has written, and either
                // null or an `NSError` it owns.
                let (url, error) = unsafe { (url.as_ref(), error.as_ref()) };
                let path = error.is_none().then(|| url.path().map(|p| PathBuf::from(p.to_string()))).flatten();
                finish(&state, path, error.map(|e| e.localizedDescription().to_string()));
            });
            // SAFETY: the destination is a file URL for a directory that exists, the options are
            // the empty dictionary the method documents, the queue is AppKit's main queue (where
            // the reader must run, since it reports into the UI), and the block has the
            // `(NSURL, NSError *)` signature the method calls it with.
            unsafe {
                receiver.receivePromisedFilesAtDestination_options_operationQueue_reader(&url, &options, &queue, &reader);
            }
        }
    }

    /// One drop's promises, so the last one to land can stand in the plain paths if none arrived.
    struct Pending {
        outstanding: usize,
        delivered: bool,
        fallback: Vec<PathBuf>,
    }

    /// Reports one received file, or the pasteboard's plain paths once every promise has failed.
    fn finish(state: &Rc<RefCell<Pending>>, path: Option<PathBuf>, error: Option<String>) {
        let mut report = Vec::new();
        {
            let Ok(mut state) = state.try_borrow_mut() else { return };
            state.outstanding = state.outstanding.saturating_sub(1);
            match path {
                Some(path) => {
                    state.delivered = true;
                    report.push(path);
                }
                None => log::warn!("a dropped file was promised but not written: {}", error.unwrap_or_else(|| "no reason given".into())),
            }
            if state.outstanding == 0 && !state.delivered {
                report.append(&mut state.fallback);
            }
        }
        deliver(report);
    }

    /// Hands files to the host to import, if there are any.
    fn deliver(files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        HANDLERS.with_borrow(|h| {
            if let Some(h) = h.as_ref() {
                (h.received)(files);
            }
        });
    }

    /// The paths a pasteboard carries outright, as winit reads them.
    fn plain_paths(pb: &NSPasteboard) -> Vec<PathBuf> {
        let kind = NSString::from_str("NSFilenamesPboardType");
        let Some(list) = pb.propertyListForType(&kind) else { return Vec::new() };
        let Ok(list) = list.downcast::<NSArray>() else { return Vec::new() };
        list.iter().filter_map(|o| o.downcast::<NSString>().ok()).map(|s| PathBuf::from(s.to_string())).collect()
    }
}

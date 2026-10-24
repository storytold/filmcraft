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
//! carries promises, and reports a drop's files once they have all landed (or what was lost, if
//! the source never produced them). Everything else (Finder, a file manager
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
    /// A drop whose files are promised, reported on the main thread as it happens, before any of
    /// them exists, with where it landed in the window: logical points from the window's top-left
    /// corner, as the UI measures them, or `None` when the drag names no window. The files can
    /// take minutes to arrive (a large iCloud asset is downloaded first), by which time the drop
    /// is long over, and the OS reports no pointer motion during a drag, so this is the only
    /// moment the position can be had. Its [`DropId`] is the one the `received` calls for those
    /// files carry.
    pub dropped: Box<dyn Fn(DropId, Option<(f64, f64)>)>,
    /// The files of one drop, on the main thread, once they have all arrived: one call per drop,
    /// in the order the drag offered them (promises are fulfilled in parallel and land in no
    /// particular order, and the files of a drop are imported as one, so the last is waited for).
    /// The host imports them as if they had been dropped, and must ask for a repaint: they arrive
    /// between frames.
    pub received: Box<dyn Fn(DropId, Vec<PathBuf>)>,
    /// Promises of a drop that were never kept, once the drop is over: how many files were lost
    /// and what the dragging source said about the first of them. Nothing stands in for them —
    /// the drag's own paths are a poster frame rather than the movie, for Photos.app, and
    /// importing that is the very thing this module exists to stop — so the host has only this to
    /// tell the user with.
    pub failed: Box<dyn Fn(DropId, usize, String)>,
}

/// Identifies one promised-file drop, so the host can match the files that eventually arrive to
/// the drop they belong to. Unique within a run; nothing is implied by its order or value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DropId(pub u64);

/// Where each drop still waiting for its files landed, held until that drop is over.
///
/// A promise can take minutes to keep (a 500 MB iCloud video is downloaded first) and any number
/// of drags can be in flight over that time, so there is no count of pending drops worth guessing
/// at: an entry goes when its drop lands or fails, and only a promise that does neither leaves
/// one behind. The position is whatever the host works in — [`Handlers::dropped`] reports window
/// points, which a host scales to its own before recording them here.
pub struct PendingDrops<P>(std::sync::Arc<std::sync::Mutex<std::collections::HashMap<DropId, Option<P>>>>);

impl<P> Clone for PendingDrops<P> {
    fn clone(&self) -> Self {
        Self(std::sync::Arc::clone(&self.0))
    }
}

impl<P> Default for PendingDrops<P> {
    fn default() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())))
    }
}

impl<P> PendingDrops<P> {
    /// Records where `id` was dropped, as the drop happens.
    pub fn dropped(&self, id: DropId, at: Option<P>) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id, at);
    }

    /// Hands back where `id` was dropped and forgets it: the drop is over either way.
    pub fn take(&self, id: DropId) -> Option<P> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id).flatten()
    }

    /// How many drops are still waiting for their files.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    /// Whether every drop taken so far is over.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
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
    use super::{DropId, Handlers};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{ClassType, MainThreadMarker, msg_send, sel};
    use objc2_app_kit::{NSApplication, NSFilePromiseReceiver, NSPasteboard, NSPasteboardType};
    use objc2_foundation::{NSArray, NSDictionary, NSError, NSOperationQueue, NSPoint, NSRect, NSString, NSURL};
    use std::cell::{Cell, RefCell};
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

    thread_local! {
        /// Hands out the [`DropId`] of each drop taken here.
        static DROPS: Cell<u64> = const { Cell::new(0) };
    }

    /// The id of the next promised drop.
    fn next_drop() -> DropId {
        DROPS.with(|n| {
            let id = n.get().wrapping_add(1);
            n.set(id);
            DropId(id)
        })
    }

    /// A dragging-destination method: `(self, _cmd, sender) -> BOOL`. winit declares
    /// `draggingEntered:` with a `BOOL` return too, where AppKit reads an `NSDragOperation`; YES
    /// is `NSDragOperationCopy`, NO is `NSDragOperationNone`, so both keep that signature.
    type DragFn = unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;

    pub fn install(handlers: Handlers) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let app = NSApplication::sharedApplication(mtm);

        let mut delegate_class: Option<&AnyClass> = None;
        for window in app.windows().iter() {
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
            if !fits(perform) || !fits(entered) {
                return false;
            }
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
        // Only now that the drops will be taken: accept the promise types as well, so a drag
        // that offers nothing but promises (and therefore no `NSFilenamesPboardType`) is not
        // rejected before it can be read. Registering these before the checks above would leave
        // a declined install advertising types winit's own `performDragOperation:` finds no
        // paths in, so a pure-promise drag would be accepted and then do nothing at all, where
        // AppKit used to refuse it outright.
        let mut types: Vec<Retained<NSPasteboardType>> = NSFilePromiseReceiver::readableDraggedTypes().iter().collect();
        types.push(NSString::from_str("NSFilenamesPboardType"));
        types.push(NSString::from_str("public.file-url"));
        let types = NSArray::from_retained_slice(&types);
        for window in app.windows().iter() {
            window.registerForDraggedTypes(&types);
        }

        HANDLERS.with_borrow_mut(|h| *h = Some(handlers));
        true
    }

    /// Whether a method is the `(self, _cmd, id) -> BOOL` the replacements are written as. A
    /// windowing layer that declared these differently (an `NSDragOperation` return, say) would
    /// otherwise be called through the wrong signature, so this is what makes a winit the
    /// interception no longer fits decline rather than misbehave.
    fn fits(method: &objc2::runtime::Method) -> bool {
        let object = method.argument_type(2).is_some_and(|t| t.to_bytes() == b"@");
        // BOOL is `bool` where the ABI has one and a `signed char` otherwise.
        let boolean = matches!(method.return_type().to_bytes(), b"B" | b"c");
        method.arguments_count() == 3 && object && boolean
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
            // Read during the drop rather than when the files land: by then the destination may
            // follow a different project than the one they were dropped on.
            let drop = TakenDrop { id: next_drop(), destination: HANDLERS.with_borrow(|h| h.as_ref().map(|h| (h.destination)()))? };
            let at = dropped_at(sender);
            HANDLERS.with_borrow(|h| {
                if let Some(h) = h.as_ref() {
                    (h.dropped)(drop.id, at);
                }
            });
            Some(after_this_event(this, receivers, drop))
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
    fn after_this_event(delegate: *mut AnyObject, receivers: Vec<Retained<NSFilePromiseReceiver>>, drop: TakenDrop) -> bool {
        if MainThreadMarker::new().is_none() {
            return false;
        }
        let Some(delegate) = std::ptr::NonNull::new(delegate) else { return false };
        // SAFETY: `delegate` is the window delegate AppKit sent `performDragOperation:` to, so it
        // is a live object, retained here for as long as the queued block needs it.
        let delegate = unsafe { Retained::retain(delegate.as_ptr()) };
        let Some(delegate) = delegate else { return false };
        let once = RefCell::new(Some((receivers, drop, delegate)));
        let block = block2::RcBlock::new(move || {
            let Some((receivers, drop, delegate)) = once.borrow_mut().take() else { return };
            // winit clears the files it reported as hovering when it sees the drop or the drag
            // leaving. It sees neither of a drop taken here, and the UI would go on believing
            // files are over the window, so tell it the drag is gone.
            let delegate: &AnyObject = &delegate;
            // SAFETY: the delegate is retained above and implements `draggingExited:`, whose
            // sender may be nil.
            unsafe { msg_send![delegate, draggingExited: std::ptr::null_mut::<AnyObject>()] }
            receive(&receivers, drop);
        });
        // SAFETY: the block takes no arguments and returns nothing, as the queue calls it, and
        // it is run on the main thread (the main queue's only thread), where everything it
        // touches belongs.
        unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
        true
    }

    /// Where a drag was let go, in logical points from the top-left corner of its window's
    /// content, which is how the UI measures a position. AppKit reports it in the window's own
    /// coordinates, whose origin is the *bottom* left, so the content's height flips it.
    ///
    /// winit delivers no cursor events while files are dragged over a window, so this is the only
    /// account of where the drop landed: without it the files are placed wherever the pointer
    /// last really moved, which for a drag that came from another application is nowhere near.
    fn dropped_at(sender: *mut AnyObject) -> Option<(f64, f64)> {
        let sender = std::ptr::NonNull::new(sender)?;
        // SAFETY: AppKit passes a non-null `NSDraggingInfo`, whose `draggingLocation` is a point
        // and whose `draggingDestinationWindow` is its window or nil.
        let (point, window): (NSPoint, *mut AnyObject) =
            unsafe { (msg_send![sender.as_ptr(), draggingLocation], msg_send![sender.as_ptr(), draggingDestinationWindow]) };
        let window = std::ptr::NonNull::new(window)?;
        // SAFETY: a non-nil `NSWindow` has a `contentView`, itself nil only before one is set,
        // and a view's `frame` is a rectangle.
        let height = unsafe {
            let view: *mut AnyObject = msg_send![window.as_ptr(), contentView];
            let view = std::ptr::NonNull::new(view)?;
            let frame: NSRect = msg_send![view.as_ptr(), frame];
            frame.size.height
        };
        Some((point.x, height - point.y))
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
    /// directory, and reports each one as it lands. Nothing stands in for a promise that fails:
    /// the drag's own paths are the poster frame this exists to stop importing, so an unusable
    /// destination or a source that gives up is reported to the user rather than papered over.
    fn receive(receivers: &[Retained<NSFilePromiseReceiver>], drop: TakenDrop) {
        let TakenDrop { id, destination } = drop;
        if let Err(e) = std::fs::create_dir_all(&destination) {
            log::warn!("dropped promised files: {} is not writable: {e}", destination.display());
            lost_files(id, receivers.len(), format!("{}: {e}", destination.display()));
            return;
        }
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&destination.to_string_lossy()), true);

        let state = Rc::new(RefCell::new(Pending { id, received: vec![None; receivers.len()], outstanding: receivers.len(), lost: 0, reason: None }));
        let queue = NSOperationQueue::mainQueue();
        let options = NSDictionary::new();
        for (offered, receiver) in receivers.iter().enumerate() {
            let state = Rc::clone(&state);
            let reader = block2::RcBlock::new(move |url: std::ptr::NonNull<NSURL>, error: *mut NSError| {
                // SAFETY: AppKit passes a non-null `NSURL` for a file it has written, and either
                // null or an `NSError` it owns.
                let (url, error) = unsafe { (url.as_ref(), error.as_ref()) };
                let path = error.is_none().then(|| url.path().map(|p| PathBuf::from(p.to_string()))).flatten();
                finish(&state, offered, path, error.map(|e| e.localizedDescription().to_string()));
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

    /// What a taken drop is worth knowing once it is over: everything here is read during the
    /// drop, because the pasteboard and the drag are gone by the time the files arrive.
    struct TakenDrop {
        id: DropId,
        destination: PathBuf,
    }

    /// One drop's promises: the files kept in the order they were offered until the last of them
    /// lands, and what became of the ones that were not.
    struct Pending {
        id: DropId,
        received: Vec<Option<PathBuf>>,
        outstanding: usize,
        lost: usize,
        reason: Option<String>,
    }

    /// Takes in the promise offered `offered`th, and once the drop's last one has landed reports
    /// the files it produced and whatever it failed to produce.
    fn finish(state: &Rc<RefCell<Pending>>, offered: usize, path: Option<PathBuf>, error: Option<String>) {
        let mut report = Vec::new();
        let (id, lost, reason);
        {
            let Ok(mut state) = state.try_borrow_mut() else { return };
            state.outstanding = state.outstanding.saturating_sub(1);
            match path {
                Some(path) => {
                    if let Some(slot) = state.received.get_mut(offered) {
                        *slot = Some(path);
                    }
                }
                None => {
                    let error = error.unwrap_or_else(|| "no reason given".into());
                    log::warn!("a dropped file was promised but not written: {error}");
                    state.lost = state.lost.saturating_add(1);
                    state.reason.get_or_insert(error);
                }
            }
            if state.outstanding > 0 {
                return;
            }
            report.extend(state.received.iter_mut().filter_map(Option::take));
            (id, lost, reason) = (state.id, state.lost, state.reason.take());
        }
        deliver(id, report);
        if let Some(reason) = reason {
            lost_files(id, lost, reason);
        }
    }

    /// Tells the host about promises the dragging source never kept. Nothing stands in for them:
    /// what the drag offers outright is a poster frame rather than the movie it promised.
    fn lost_files(id: DropId, files: usize, reason: String) {
        if files == 0 {
            return;
        }
        HANDLERS.with_borrow(|h| {
            if let Some(h) = h.as_ref() {
                (h.failed)(id, files, reason.clone());
            }
        });
    }

    /// Hands files to the host to import, if there are any.
    fn deliver(id: DropId, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        HANDLERS.with_borrow(|h| {
            if let Some(h) = h.as_ref() {
                (h.received)(id, files);
            }
        });
    }

    /// What a drop reports is decided here rather than by AppKit, so it can be tested without a
    /// drag: the promises are handed to [`finish`] exactly as their readers would, in whatever
    /// order they land.
    #[cfg(test)]
    mod tests {
        use super::*;

        /// What `finish` reports for a drop whose promises land in `landing` order, each either
        /// a path or a failure: the files imported, and the (count, reason) of what was lost.
        #[allow(clippy::type_complexity)]
        fn reported(landing: &[(usize, Option<&str>)]) -> (Vec<Vec<PathBuf>>, Vec<(usize, String)>) {
            let files = Rc::new(RefCell::new(Vec::new()));
            let losses = Rc::new(RefCell::new(Vec::new()));
            let (seen, missing) = (Rc::clone(&files), Rc::clone(&losses));
            HANDLERS.with_borrow_mut(|h| {
                *h = Some(Handlers {
                    destination: Box::new(PathBuf::new),
                    dropped: Box::new(|_, _| {}),
                    received: Box::new(move |_, files| seen.borrow_mut().push(files)),
                    failed: Box::new(move |_, count, reason| missing.borrow_mut().push((count, reason))),
                })
            });
            let state =
                Rc::new(RefCell::new(Pending { id: next_drop(), received: vec![None; landing.len()], outstanding: landing.len(), lost: 0, reason: None }));
            for (offered, path) in landing {
                finish(&state, *offered, path.map(PathBuf::from), path.is_none().then(|| "offline".to_string()));
            }
            HANDLERS.with_borrow_mut(|h| *h = None);
            (files.borrow().clone(), losses.borrow().clone())
        }

        /// A drop is one import, so its files are reported together once the last has landed —
        /// in the order the drag offered them, not the order they finished downloading in.
        #[test]
        fn a_drops_files_are_reported_once_in_the_order_they_were_offered() {
            let (imported, lost) = reported(&[(2, Some("/tmp/third.mov")), (0, Some("/tmp/first.mov")), (1, Some("/tmp/second.mov"))]);
            assert_eq!(imported, vec![vec![PathBuf::from("/tmp/first.mov"), PathBuf::from("/tmp/second.mov"), PathBuf::from("/tmp/third.mov")]]);
            assert!(lost.is_empty());
        }

        /// A promise that fails (an iCloud asset with no connection to download it) costs its own
        /// file and is reported as lost; the files that did arrive are imported.
        #[test]
        fn a_failed_promise_leaves_the_files_that_did_arrive() {
            let (imported, lost) = reported(&[(0, None), (1, Some("/tmp/second.mov"))]);
            assert_eq!(imported, vec![vec![PathBuf::from("/tmp/second.mov")]]);
            assert_eq!(lost, vec![(1, "offline".to_string())], "the user is told what did not arrive");
        }

        /// Nothing stands in for a promise the source never kept: the drag's own paths are the
        /// poster frame rather than the movie, and importing that is the bug this module fixes.
        #[test]
        fn a_drop_whose_promises_all_fail_imports_nothing_and_says_so() {
            let (imported, lost) = reported(&[(0, None), (1, None)]);
            assert!(imported.is_empty(), "no stand-in is imported");
            assert_eq!(lost, vec![(2, "offline".to_string())]);
        }
    }
}

#[cfg(test)]
mod pending_tests {
    use super::{DropId, PendingDrops};

    /// Any number of drags can be in flight, and the one that started first is as likely as any to
    /// be the one still downloading: its position has to outlive every drop that overtakes it,
    /// however many of those are outstanding at once.
    #[test]
    fn a_slow_drops_position_outlives_the_drops_that_overtake_it() {
        let pending = PendingDrops::default();
        let slow = DropId(0);
        pending.dropped(slow, Some((120.0, 480.0)));

        // Every one of these is still downloading too, so they are all outstanding together: a
        // store that only kept the newest few would have let go of the slow drop long before the
        // last of them was recorded.
        for later in 1..=64 {
            pending.dropped(DropId(later), Some((later as f64, 0.0)));
        }
        assert_eq!(pending.len(), 65, "a pending drop was forgotten before it was over");

        assert_eq!(pending.take(slow), Some((120.0, 480.0)), "the slow drop forgot where it landed");
        for later in 1..=64 {
            assert_eq!(pending.take(DropId(later)), Some((later as f64, 0.0)), "drop {later} forgot where it landed");
        }
        assert!(pending.is_empty(), "a drop that is over is still remembered");
    }

    /// Nothing is left behind by a drop that lands, one that fails, or one with no position at all.
    #[test]
    fn a_drop_that_is_over_is_forgotten() {
        let pending: PendingDrops<(f64, f64)> = PendingDrops::default();
        pending.dropped(DropId(1), Some((10.0, 10.0)));
        pending.dropped(DropId(2), None);
        assert_eq!(pending.len(), 2);

        assert_eq!(pending.take(DropId(2)), None, "a drop with no position reported one");
        assert_eq!(pending.take(DropId(1)), Some((10.0, 10.0)));
        assert!(pending.is_empty());
        assert_eq!(pending.take(DropId(1)), None, "a drop was reported twice");
    }
}

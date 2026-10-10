//! macOS "open documents" Apple Event handler (FFI half of [`crate::open_documents`]).
//!
//! AppKit installs its own handler for `kAEOpenDocuments` while the app finishes launching; with
//! no `NSDocument` classes and a winit delegate that does not implement `application:openURLs:`,
//! that handler answers every document with "FilmCraft cannot open files of this type". Apple's
//! documented place to replace it is `applicationWillFinishLaunching:`, which belongs to winit's
//! delegate, so we observe `NSApplicationWillFinishLaunchingNotification` instead (posted at the
//! same point) and install our handler there. It is also installed right away, which covers a
//! call made after launch. The handler only converts the event's file URLs to paths and queues
//! them; it never panics across the FFI boundary (`catch_unwind`).

use std::panic::{AssertUnwindSafe, catch_unwind};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSString};

/// Four-character codes (Carbon `AE/AERegistry.h`).
const K_CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const K_AE_OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
/// At most this many documents from one event.
const MAX_ITEMS: isize = 1024;

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "FilmCraftOpenDocumentsHandler"]
    struct Handler;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for Handler {}

    impl Handler {
        // SAFETY: the signature matches the selector registered with NSNotificationCenter below
        // (one object argument, no return value).
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _note: &NSNotification) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.register()));
        }

        // SAFETY: the signature is the one NSAppleEventManager requires for an event handler
        // selector: `- (void)handleEvent:(NSAppleEventDescriptor *)event withReplyEvent:(NSAppleEventDescriptor *)reply`.
        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            if catch_unwind(AssertUnwindSafe(|| crate::open_documents::push(paths(event)))).is_err() {
                log::error!("open documents: the handler panicked; the request was ignored");
            }
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: `init` is NSObject's designated initialiser, and the class adds no ivars.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }

    /// Make this object the `kAEOpenDocuments` handler (replacing AppKit's).
    fn register(&self) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: the selector names `handleOpenDocuments:withReplyEvent:` defined above with the
        // required signature; the handler object is leaked in `install`, so it outlives the
        // registration (NSAppleEventManager does not retain it).
        unsafe {
            manager.setEventHandler_andSelector_forEventClass_andEventID(
                self,
                sel!(handleOpenDocuments:withReplyEvent:),
                K_CORE_EVENT_CLASS,
                K_AE_OPEN_DOCUMENTS,
            );
        }
    }
}

/// File paths in an open-documents event: its direct object is a list of file references (or a
/// single one); anything that is not a file URL is skipped.
fn paths(event: &NSAppleEventDescriptor) -> Vec<std::path::PathBuf> {
    let Some(direct) = event.paramDescriptorForKeyword(KEY_DIRECT_OBJECT) else { return Vec::new() };
    let n = direct.numberOfItems().clamp(0, MAX_ITEMS);
    let items: Vec<Retained<NSAppleEventDescriptor>> = if n == 0 { vec![direct] } else { (1..=n).filter_map(|i| direct.descriptorAtIndex(i)).collect() };
    items.iter().filter_map(|d| d.fileURLValue()).filter_map(|u| u.to_file_path()).collect()
}

/// Install the handler (see the module docs). Must run on the main thread.
pub(crate) fn install() -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("open documents: install must run on the main thread")?;
    let handler = Handler::new(mtm);
    handler.register();
    let center = NSNotificationCenter::defaultCenter();
    // The notification's name is its symbol name; a string avoids reading AppKit's extern static.
    let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
    // SAFETY: the selector names `applicationWillFinishLaunching:` defined above, taking the
    // notification; the observer is leaked below, so it is never deallocated while registered.
    unsafe {
        center.addObserver_selector_name_object(&handler, sel!(applicationWillFinishLaunching:), Some(&name), None);
    }
    // One handler for the life of the process: neither the Apple Event manager nor the
    // notification centre keeps it alive.
    std::mem::forget(handler);
    Ok(())
}

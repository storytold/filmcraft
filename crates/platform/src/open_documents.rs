//! The documents the operating system asks a running app to open (macOS open events, #1229).
//!
//! A `.fcproj` double-clicked in Finder — or `open -a FilmCraft …` — reaches the app as the Apple
//! Event `aevt`/`odoc`, and only once the bundle claims the format with `CFBundleDocumentTypes`
//! (`packaging/macos/Info.plist.in`): without that claim Launch Services refuses the file with
//! "FilmCraft cannot open files in the 'FilmCraft Project' format" and never starts the app.
//!
//! The claim alone still leaves the project unopened. AppKit hands the event to the *application
//! delegate*, and the delegate belongs to the windowing layer (`winit` registers its own
//! `WinitApplicationDelegate` and panics when it finds another one there), so the event was
//! dropped and FilmCraft came up with its default project. This module registers its own `odoc`
//! handler with `NSAppleEventManager`, which replaces AppKit's handling of that one event without
//! touching the delegate. [`install`] says where and why that registration has to happen twice for
//! the event that *launched* the app to be caught as well as one handed to a running app.
//!
//! This is the crate's second non-media OS call (`cursor.rs` is the first): the handler is an
//! Objective-C object, so it lives here rather than in the UI (AGENTS.md §0.3, ADR 0001). The
//! handler never blocks and never panics (AGENTS.md §0); the API is safe — paths go in as
//! `String`s, the handler appends them to a queue, asks the caller's `wake` for a frame, and
//! [`take`] hands them out.

use std::sync::{Mutex, OnceLock, PoisonError};

/// The paths a document open handed over, waiting for the frame that opens them (oldest first).
static OPENED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The frame loop to wake when paths arrive (set by [`install`]): a window that is idle only
/// notices the queue on its next unrelated frame, so the event asks for a repaint.
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// The documents the operating system handed over since the last call, oldest first. One call per
/// frame is enough, and a path is never handed out twice.
///
/// Empty where the platform has no open events to drain (Windows and Linux start the app with the
/// files as arguments) or while [`install`] has not run.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Take the platform's document open events over, so a project opened from outside the app really
/// opens. Returns whether this platform delivers such events at all, which tells a caller why a
/// double-click does nothing where the files arrive as arguments instead.
///
/// Call once, from the main thread, from `main` before the event loop runs: macOS dispatches the
/// event that *launched* the app while it is finishing launching, before any handler installed
/// from the app itself can run, and this is what gets there in time. An app that is already
/// running has its handler from this call too, so documents handed to it arrive the same way.
pub fn install() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::install()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The frame loop to ask for a frame when documents arrive (the UI opens them, [`take`]ing the
/// paths once a frame). Without it the paths still queue and are opened by the next frame drawn
/// for any other reason.
pub fn set_wake(wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
}

/// Hand `paths` to the frame loop. Called by the operating system's event handler, so it must not
/// block and must not panic: the lock is taken with the poison guard (AGENTS.md §0.6) and `wake`
/// runs outside it.
#[cfg(target_os = "macos")]
fn deliver(paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    // Worth a record: this is the one way a project reaches the app without the user opening it
    // inside the app, so a report of "double-clicking does nothing" starts here.
    log::info!("the operating system handed over {} document(s)", paths.len());
    OPENED.lock().unwrap_or_else(PoisonError::into_inner).extend(paths);
    if let Some(wake) = WAKE.get() {
        wake();
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    //! The Apple Event half: an Objective-C handler object and the four-character codes it reads.

    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSString};

    // The handler AppKit calls for `odoc`. AppKit does not retain its handler, and the main thread
    // lives as long as the process, so it is kept for the process here.
    thread_local! {
        static HANDLER: RefCell<Option<Retained<OpenDocumentsHandler>>> = const { RefCell::new(None) };
    }

    /// `kCoreEventClass` (`'aevt'`) and `kAEOpenDocuments` (`'odoc'`): the event macOS sends an app
    /// it wants to open documents with. `keyDirectObject` (`'----'`) holds its file list, and
    /// `typeAEList` (`'list'`) and `typeFileURL` (`'furl'`) describe what is inside it.
    ///
    /// The four-character codes are Apple Event ABI and never change, so they are spelled out here:
    /// the typed constants live in `objc2-core-services`, a whole framework binding for the four
    /// calls below.
    const CORE_EVENT_CLASS: u32 = four_cc(*b"aevt");
    const OPEN_DOCUMENTS: u32 = four_cc(*b"odoc");
    const KEY_DIRECT_OBJECT: u32 = four_cc(*b"----");
    const TYPE_FILE_URL: u32 = four_cc(*b"furl");

    const fn four_cc(code: [u8; 4]) -> u32 {
        u32::from_be_bytes(code)
    }

    define_class!(
        /// The `aevt`/`odoc` handler: AppKit calls `handleEvent:withReplyEvent:` on the main thread
        /// with the event and a descriptor to answer on (the sender does not wait for the answer,
        /// so no reply is written).
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "FilmCraftOpenDocumentsHandler"]
        struct OpenDocumentsHandler;

        impl OpenDocumentsHandler {
            /// The open event itself: the files to open, on the main thread.
            #[unsafe(method(handleEvent:withReplyEvent:))]
            fn handle_event(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                super::deliver(paths_of(event));
            }

            /// `NSApplicationWillFinishLaunchingNotification`: register the `odoc` handler again,
            /// over the one AppKit installs for itself while the application is created, and before
            /// the event that launched the app is dispatched.
            #[unsafe(method(applicationWillFinishLaunching:))]
            fn will_finish_launching(&self, _notification: &NSNotification) {
                register(self);
            }
        }

        unsafe impl NSObjectProtocol for OpenDocumentsHandler {}
    );

    /// Install the `odoc` handler, once per process: a second call only registers it again.
    ///
    /// The handler is registered twice on purpose, and both registrations are load-bearing. AppKit
    /// installs a handler of its own for `odoc` while the application object is created, which
    /// replaces one registered before that — registering only in `main`, before the event loop,
    /// catches nothing at all (measured on 2026-10-12: a project launched with the app and a
    /// project handed to the running app both stayed unopened). The event that *launched* the app
    /// is then dispatched between `applicationWillFinishLaunching` and `applicationDidFinishLaunching`,
    /// so a handler registered as late as the app creator is too late for it as well. Registering
    /// right away covers a host that installs after the app is up, and registering again from the
    /// will-finish-launching notification is what actually survives AppKit and lands before the
    /// launch event.
    pub(super) fn install() -> bool {
        // The Apple Event manager, the notification centre and this class are main-thread only.
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let handler = match HANDLER.with(|h| h.borrow().clone()) {
            Some(handler) => handler,
            None => {
                let handler = OpenDocumentsHandler::alloc(mtm).set_ivars(());
                // SAFETY: `handler` is a fresh allocation whose ivars are the unit type (nothing to
                // initialize); `init` is NSObject's initializer, which returns the same object.
                let handler: Retained<OpenDocumentsHandler> = unsafe { msg_send![super(handler), init] };
                observe_will_finish_launching(&handler);
                HANDLER.with(|h| *h.borrow_mut() = Some(handler.clone()));
                handler
            }
        };
        register(&handler);
        true
    }

    /// Register `handler` as the `odoc` handler, replacing AppKit's default one.
    fn register(handler: &OpenDocumentsHandler) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: `manager` is the live shared event manager, `handler` a live instance of the
        // class above (which implements exactly this selector: two descriptors, as AppKit calls
        // it), and the codes are the `aevt`/`odoc` constants AppKit dispatches the event with.
        unsafe {
            let _: () = msg_send![
                &manager,
                setEventHandler: handler,
                andSelector: sel!(handleEvent:withReplyEvent:),
                forEventClass: CORE_EVENT_CLASS,
                andEventID: OPEN_DOCUMENTS,
            ];
        }
    }

    /// Ask the notification centre for `applicationWillFinishLaunching`, where the handler is
    /// registered again over AppKit's own (see [`install`]).
    fn observe_will_finish_launching(handler: &OpenDocumentsHandler) {
        let center = NSNotificationCenter::defaultCenter();
        // The name is `NSApplicationWillFinishLaunchingNotification`. Spelled out rather than
        // imported: the constant lives in `objc2-app-kit`, which this crate would otherwise not
        // need, and the notification centre matches names by string (`isEqual:`), retaining this
        // one for as long as the observation lives.
        let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
        // SAFETY: the centre is global, `handler` outlives the observation (it is kept for the
        // process), the selector is one the class above declares, and a nil object means "from any
        // sender".
        unsafe {
            let _: () = msg_send![
                &center,
                addObserver: handler,
                selector: sel!(applicationWillFinishLaunching:),
                name: &*name,
                object: std::ptr::null::<NSObject>(),
            ];
        }
    }

    /// The paths of an `odoc` event, in the order the sender listed them.
    fn paths_of(event: &NSAppleEventDescriptor) -> Vec<String> {
        let Some(files) = direct_object(event) else { return Vec::new() };
        // An Apple Event list is 1-based, and a descriptor that is not a list counts as its own
        // single item, so this reads a lone file and a list of them alike.
        let mut paths = Vec::new();
        for index in 1..=files.numberOfItems() {
            if let Some(path) = files.descriptorAtIndex(index).and_then(|file| file_path(&file)) {
                paths.push(path);
            }
        }
        paths
    }

    /// The event's direct object (`keyDirectObject`): the files to open.
    fn direct_object(event: &NSAppleEventDescriptor) -> Option<Retained<NSAppleEventDescriptor>> {
        // SAFETY: `event` is a live descriptor from AppKit; the method takes a four-character code
        // by value and returns the parameter descriptor (or nil), which objc2 retains.
        unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] }
    }

    /// A descriptor's four-character type (`'furl'`, `'alis'`, `'list'`, …).
    fn descriptor_type(descriptor: &NSAppleEventDescriptor) -> u32 {
        // SAFETY: the method takes no arguments and returns the descriptor's type by value.
        unsafe { msg_send![descriptor, descriptorType] }
    }

    /// `descriptor` coerced to a file URL, or `None` when it does not describe a file.
    fn as_file_url(descriptor: &NSAppleEventDescriptor) -> Option<Retained<NSAppleEventDescriptor>> {
        // SAFETY: `descriptor` is live; the coercion returns a new descriptor (or nil), which objc2
        // retains.
        unsafe { msg_send![descriptor, coerceToDescriptorType: TYPE_FILE_URL] }
    }

    /// One entry of the event as a path: a file URL as sent, or an alias (`'alis'`) or any older
    /// descriptor coerced to one. Anything that is not a file URL — a web URL a caller passed as a
    /// document — is not a file to open and is left out.
    fn file_path(descriptor: &NSAppleEventDescriptor) -> Option<String> {
        let url = if descriptor_type(descriptor) == TYPE_FILE_URL { descriptor.fileURLValue() } else { as_file_url(descriptor)?.fileURLValue() }?;
        if !url.isFileURL() {
            return None;
        }
        url.path().map(|path| path.to_string())
    }

    #[cfg(test)]
    mod tests {
        /// The codes are read big-endian, as Apple Event's four-character codes are defined.
        #[test]
        fn four_character_codes_are_big_endian() {
            assert_eq!(super::CORE_EVENT_CLASS, 0x6165_7674, "'aevt'");
            assert_eq!(super::OPEN_DOCUMENTS, 0x6f64_6f63, "'odoc'");
            assert_eq!(super::KEY_DIRECT_OBJECT, 0x2d2d_2d2d, "'----'");
            assert_eq!(super::TYPE_FILE_URL, 0x6675_726c, "'furl'");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{deliver, take};

    /// The queue the event handler fills and the frame loop drains: paths come out once, in the
    /// order they arrived, and nothing arrives without a document.
    #[test]
    fn paths_are_handed_over_once_and_in_order() {
        assert!(take().is_empty(), "nothing to open before an event");
        deliver(Vec::new());
        assert!(take().is_empty(), "an empty event opens nothing");
        deliver(vec!["/a/cut.fcproj".to_string()]);
        deliver(vec!["/b/take.fcproj".to_string(), "/b/voice.wav".to_string()]);
        assert_eq!(take(), ["/a/cut.fcproj", "/b/take.fcproj", "/b/voice.wav"]);
        assert!(take().is_empty(), "a path is handed over once");
    }
}

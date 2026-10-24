//! Files dropped on the main window from a file manager, on Wayland (Linux).
//!
//! winit 0.30's Wayland backend implements no drag-and-drop, so eframe never reports hovered or
//! dropped files there. [`WaylandDrops`] receives them itself: it joins the window's own Wayland
//! connection (libwayland, the same `wl_display` winit opened) with a separate event queue on a
//! worker thread, creates a `wl_data_device` for every seat, and accepts `text/uri-list` offers
//! over the window's `wl_surface`, the way egui's clipboard (`smithay-clipboard`) shares the
//! connection. Drags elsewhere and other formats are declined. The paths come out through
//! [`crate::uri_list`].
//!
//! The one `unsafe` call borrows winit's `wl_display` ([`borrow_display`]; ADR 0002). It is sound
//! because the borrow ends before winit's connection does: dropping [`WaylandDrops`] stops and
//! joins the worker, which is the only user of the borrowed connection, and the app holding it is
//! dropped inside eframe's event loop, before winit closes the display. The window's surface is
//! only compared by address, never dereferenced.
//!
//! Remove this module once eframe moves to winit 0.31, which delivers Wayland drops itself.

use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{self, DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child};

const URI_LIST: &str = "text/uri-list";
/// A file manager that has not sent the whole list after this long is given up on.
const READ_DEADLINE: Duration = Duration::from_secs(5);
/// How long the worker waits on the list before looking at shutdown and Wayland events again.
const READ_SLICE: Duration = Duration::from_millis(50);
/// A uri-list longer than this is cut off (about 10 000 paths).
const MAX_LIST_BYTES: usize = 4 << 20;

/// A file drag over the main window. Positions are in the window's logical pixels (winit's
/// logical coordinates: physical pixels divided by the window's scale factor).
#[derive(Clone, Debug, PartialEq)]
pub enum DragEvent {
    /// Files are being dragged over the window, now at this position.
    Hover { x: f64, y: f64 },
    /// The drag left the window or was cancelled.
    Leave,
    /// Files were dropped at this position.
    Drop { x: f64, y: f64, paths: Vec<PathBuf> },
}

/// Receives file drops on a Wayland window; see the module docs. Dropping it stops the worker.
pub struct WaylandDrops {
    events: Receiver<DragEvent>,
    stop: Arc<AtomicBool>,
    conn: Connection,
    queue: QueueHandle<State>,
    worker: Option<JoinHandle<()>>,
}

impl WaylandDrops {
    /// Starts receiving drops on `window` (eframe's `CreationContext`). `wake` is called from the
    /// worker after each new event, to ask the UI for a frame. An error (not a Wayland window, no
    /// data-device support, no thread) means drops stay unavailable; nothing else changes.
    pub fn start(window: &(impl HasWindowHandle + HasDisplayHandle), wake: Box<dyn Fn() + Send>) -> Result<Self, String> {
        let display = match window.display_handle().map_err(|e| e.to_string())?.as_raw() {
            RawDisplayHandle::Wayland(h) => h.display,
            _ => return Err("not a Wayland window".into()),
        };
        let surface = match window.window_handle().map_err(|e| e.to_string())?.as_raw() {
            RawWindowHandle::Wayland(h) => h.surface.as_ptr() as usize,
            _ => return Err("not a Wayland window".into()),
        };
        let conn = borrow_display(display);
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| format!("Wayland registry: {e}"))?;
        let qh = queue.handle();
        let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).map_err(|e| format!("no wl_data_device_manager: {e}"))?;
        let (tx, events) = channel();
        let mut state = State { surface, manager, devices: Vec::new(), drag: None, reading: None, tx, wake };
        let seats: Vec<(u32, u32)> =
            globals.contents().with_list(|list| list.iter().filter(|g| g.interface == WlSeat::interface().name).map(|g| (g.name, g.version)).collect());
        for (name, version) in seats {
            state.add_seat(globals.registry(), name, version, &qh);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (conn, stop) = (conn.clone(), stop.clone());
            std::thread::Builder::new().name("wayland-drops".into()).spawn(move || run(conn, queue, state, stop)).map_err(|e| format!("no drop thread: {e}"))?
        };
        Ok(Self { events, stop, conn, queue: qh, worker: Some(worker) })
    }

    /// The events since the last call, oldest first.
    pub fn poll(&self) -> Vec<DragEvent> {
        self.events.try_iter().collect()
    }
}

impl Drop for WaylandDrops {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // A round trip on the worker's queue wakes it from `blocking_dispatch` to see `stop`.
        let _ = self.conn.display().sync(&self.queue, ());
        let _ = self.conn.flush();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::warn!("Wayland drop thread panicked");
        }
    }
}

/// winit's `wl_display` as a `Connection` that does not own it.
#[allow(unsafe_code)]
fn borrow_display(display: std::ptr::NonNull<std::ffi::c_void>) -> Connection {
    // SAFETY: `display` is the live `wl_display*` of the eframe window we were handed (a Wayland
    // display handle from winit, which keeps it open for the whole event loop). A foreign backend
    // never disconnects it. Every use ends before winit disconnects it: the backend is used only by
    // the worker and by `WaylandDrops`, whose `Drop` joins the worker, and the app owning it is
    // dropped inside eframe's event loop (module docs, ADR 0002).
    let backend = unsafe { wayland_client::backend::Backend::from_foreign_display(display.as_ptr().cast()) };
    Connection::from_backend(backend)
}

/// The worker: dispatch the queue until asked to stop or the connection fails, reading a dropped
/// list in between. A panic in here ends drops for this run, not the app.
fn run(conn: Connection, mut queue: EventQueue<State>, mut state: State, stop: Arc<AtomicBool>) {
    let looped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while !stop.load(Ordering::Acquire) {
            let dispatched = if state.reading.is_some() {
                state.read_some(&conn);
                queue.dispatch_pending(&mut state)
            } else {
                queue.blocking_dispatch(&mut state)
            };
            if let Err(e) = dispatched {
                log::warn!("Wayland drops stopped: {e}");
                break;
            }
            let _ = conn.flush();
        }
    }));
    if looped.is_err() {
        log::warn!("Wayland drops stopped after a panic");
    }
    // Leave nothing pending on the shared connection.
    state.abandon_drag();
    state.abandon_read();
    let _ = conn.flush();
}

struct State {
    /// Address of the window's `wl_surface` (winit's proxy), to recognise drags over it.
    surface: usize,
    manager: WlDataDeviceManager,
    devices: Vec<(u32, WlSeat, WlDataDevice)>,
    drag: Option<Drag>,
    reading: Option<Reading>,
    tx: Sender<DragEvent>,
    wake: Box<dyn Fn() + Send>,
}

/// The offer under the pointer during a drag.
struct Drag {
    offer: Option<WlDataOffer>,
    /// Over our window, with a uri-list we accepted.
    ours: bool,
    x: f64,
    y: f64,
}

/// A dropped offer whose list is being read.
struct Reading {
    offer: WlDataOffer,
    pipe: UnixStream,
    list: Vec<u8>,
    deadline: Instant,
    x: f64,
    y: f64,
}

/// Formats an offer advertised (only whether the uri-list is among them matters).
#[derive(Default)]
struct OfferData {
    uri_list: AtomicBool,
    /// The compositor's last `wl_data_offer.action` (`DndAction` bits); 0 until one arrives.
    action: AtomicU32,
}

/// Whether a dropped offer may be told the drop finished. `finish` is a protocol error before the
/// compositor has chosen an action (and the error would close winit's connection, the app's), and
/// it tells the source the drop succeeded: after a `move` the file manager would delete the files
/// we import by path. So only a version-3 offer whose action is `copy` is finished; every other
/// offer is just destroyed, which the source takes as cancelled and keeps its files.
fn may_finish(version: u32, action: u32) -> bool {
    version >= 3 && action == DndAction::Copy.bits()
}

impl State {
    fn add_seat(&mut self, registry: &WlRegistry, name: u32, version: u32, qh: &QueueHandle<Self>) {
        let seat: WlSeat = registry.bind(name, version.clamp(1, 5), qh, ());
        let device = self.manager.get_data_device(&seat, qh, ());
        self.devices.push((name, seat, device));
    }

    fn send(&self, event: DragEvent) {
        if self.tx.send(event).is_ok() {
            (self.wake)();
        }
    }

    fn abandon_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            if let Some(offer) = drag.offer {
                offer.destroy();
            }
            if drag.ours {
                self.send(DragEvent::Leave);
            }
        }
    }

    fn abandon_read(&mut self) {
        if let Some(r) = self.reading.take() {
            r.offer.destroy();
            self.send(DragEvent::Leave);
        }
    }

    fn dropped(&mut self, conn: &Connection) {
        let Some(drag) = self.drag.take() else { return };
        let Some(offer) = drag.offer else { return };
        if !drag.ours {
            offer.destroy();
            return;
        }
        self.abandon_read();
        // The file manager writes the list into one end; libwayland sends a duplicate of the fd,
        // so ours can be dropped right away and EOF means the list is complete.
        let pipe = match UnixStream::pair() {
            Ok((ours, theirs)) => {
                offer.receive(URI_LIST.into(), theirs.as_fd());
                let _ = conn.flush();
                drop(theirs);
                ours
            }
            Err(e) => {
                log::warn!("no pipe for a dropped file list: {e}");
                offer.destroy();
                self.send(DragEvent::Leave);
                return;
            }
        };
        if let Err(e) = pipe.set_read_timeout(Some(READ_SLICE)) {
            log::warn!("dropped file list: {e}");
        }
        self.reading = Some(Reading { offer, pipe, list: Vec::new(), deadline: Instant::now() + READ_DEADLINE, x: drag.x, y: drag.y });
    }

    /// Reads what has arrived of a dropped list (at most one read slice); finishes the drop at EOF.
    fn read_some(&mut self, conn: &Connection) {
        let Some(r) = self.reading.as_mut() else { return };
        let mut buf = [0u8; 16 * 1024];
        let done = match r.pipe.read(&mut buf) {
            Ok(0) => true,
            Ok(n) => {
                let room = MAX_LIST_BYTES.saturating_sub(r.list.len());
                r.list.extend_from_slice(buf.get(..n.min(room)).unwrap_or_default());
                let full = r.list.len() >= MAX_LIST_BYTES;
                if full {
                    keep_complete_lines(&mut r.list);
                }
                full
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => false,
            Err(e) => {
                log::warn!("dropped file list: {e}");
                true
            }
        };
        if !done && Instant::now() < r.deadline {
            return;
        }
        let Some(r) = self.reading.take() else { return };
        let action = r.offer.data::<OfferData>().map_or(0, |d| d.action.load(Ordering::Relaxed));
        if may_finish(r.offer.version(), action) {
            r.offer.finish();
        } else {
            log::debug!("dropped file list read; offer not finished (version {}, action {action:#x})", r.offer.version());
        }
        r.offer.destroy();
        let _ = conn.flush();
        let paths = crate::uri_list::file_paths(&r.list);
        self.send(if paths.is_empty() { DragEvent::Leave } else { DragEvent::Drop { x: r.x, y: r.y, paths } });
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } if interface == WlSeat::interface().name => {
                state.add_seat(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.devices.retain(|(n, seat, device)| {
                    if *n == name {
                        if device.version() >= 2 {
                            device.release();
                        }
                        if seat.version() >= 5 {
                            seat.release();
                        }
                    }
                    *n != name
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: wl_data_device_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlCallback, ()> for State {
    fn event(_: &mut Self, _: &WlCallback, _: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataOffer, OfferData> for State {
    fn event(_: &mut Self, _: &WlDataOffer, event: wl_data_offer::Event, data: &OfferData, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_offer::Event::Offer { mime_type } if mime_type == URI_LIST => data.uri_list.store(true, Ordering::Relaxed),
            wl_data_offer::Event::Action { dnd_action } => {
                let bits = match dnd_action {
                    WEnum::Value(a) => a.bits(),
                    WEnum::Unknown(_) => 0,
                };
                data.action.store(bits, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferData::default())
    ]);

    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), conn: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::Enter { serial, surface, x, y, id } => {
                state.abandon_drag();
                let over_window = surface.id().as_ptr() as usize == state.surface;
                let uri_list = id.as_ref().and_then(|o| o.data::<OfferData>()).is_some_and(|d| d.uri_list.load(Ordering::Relaxed));
                let ours = over_window && uri_list;
                if let Some(offer) = &id {
                    offer.accept(serial, ours.then(|| URI_LIST.to_string()));
                    if offer.version() >= 3 {
                        let action = if ours { DndAction::Copy } else { DndAction::empty() };
                        offer.set_actions(action, action);
                    }
                }
                state.drag = Some(Drag { offer: id, ours, x, y });
                if ours {
                    state.send(DragEvent::Hover { x, y });
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if let Some(drag) = state.drag.as_mut() {
                    drag.x = x;
                    drag.y = y;
                    if drag.ours {
                        state.send(DragEvent::Hover { x, y });
                    }
                }
            }
            wl_data_device::Event::Leave => state.abandon_drag(),
            wl_data_device::Event::Drop => state.dropped(conn),
            // Clipboard offers are smithay-clipboard's business; ours would only leak.
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }
}

/// A list cut off at [`MAX_LIST_BYTES`] keeps only its complete lines: a URI cut short can still
/// name a file (`…/video.mp4` → `…/vid`), and the drop would import that one instead.
fn keep_complete_lines(list: &mut Vec<u8>) {
    let end = list.iter().rposition(|&b| b == b'\n').map_or(0, |i| i.saturating_add(1));
    list.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_cut_at_the_cap_drops_its_partial_last_line() {
        let mut list = b"file:///tmp/a.mp4\r\nfile:///home/u/vid".to_vec();
        keep_complete_lines(&mut list);
        assert_eq!(crate::uri_list::file_paths(&list), [std::path::PathBuf::from("/tmp/a.mp4")]);
        let mut one_partial = b"file:///home/u/vid".to_vec();
        keep_complete_lines(&mut one_partial);
        assert!(one_partial.is_empty());
        let mut complete = b"file:///tmp/a.mp4\n".to_vec();
        keep_complete_lines(&mut complete);
        assert_eq!(complete, b"file:///tmp/a.mp4\n");
    }

    #[test]
    fn only_a_copy_chosen_by_the_compositor_is_finished() {
        let copy = DndAction::Copy.bits();
        assert!(may_finish(3, copy));
        // No action received yet: finishing would be a protocol error on the shared connection.
        assert!(!may_finish(3, 0));
        // A move would make the file manager delete the files we import by path.
        assert!(!may_finish(3, DndAction::Move.bits()));
        assert!(!may_finish(3, DndAction::Ask.bits()));
        assert!(!may_finish(3, copy | DndAction::Move.bits()));
        // Version 1 and 2 offers have no `finish`.
        assert!(!may_finish(2, copy));
    }
}

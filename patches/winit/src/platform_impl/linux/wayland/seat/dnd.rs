//! Receiving file drags from other applications (`wl_data_device`).
//!
//! FilmCraft patch: winit 0.30 has no Wayland drag and drop, so a file manager's drag over the
//! window did nothing. This reports it the way the X11 backend does: [`WindowEvent::HoveredFile`]
//! for each file while the drag is over the window, [`WindowEvent::DroppedFile`] on drop and
//! [`WindowEvent::HoveredFileCancelled`] when the drag leaves. The pointer sends nothing during a
//! drag, so the drag position is reported as [`WindowEvent::CursorMoved`].
//!
//! Only drags that offer `text/uri-list` are taken, only for local `file:` URIs, and only with
//! the Copy action, so the source never deletes a file because of us.
//!
//! The list arrives through a pipe. Its read end is a calloop source, so a slow source never
//! blocks the event loop: when it is readable one `read` returns at once. The reading side is a
//! duplicate of the descriptor (`std` only, no `unsafe`); the original is what calloop polls.

use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd;
use std::path::PathBuf;

use calloop::{PostAction, RegistrationToken};
use sctk::compositor::SurfaceData;
use sctk::data_device_manager::WritePipe;
use sctk::data_device_manager::data_device::{DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use tracing::warn;

use crate::dpi::LogicalPosition;
use crate::event::WindowEvent;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

const URI_LIST: &str = "text/uri-list";

/// A list longer than this is not a list of files somebody dragged.
const MAX_LIST_BYTES: usize = 4 << 20;

/// The drag in progress over one of our windows.
#[derive(Debug)]
pub struct DndState {
    window_id: WindowId,
    data_device: WlDataDevice,
    /// Set while the uri-list is being read.
    reader: Option<RegistrationToken>,
    /// What has been read of the list so far.
    list: Vec<u8>,
    /// The files, once the list is complete (empty when it couldn't be read).
    files: Option<Vec<PathBuf>>,
    /// The drop happened, possibly before the whole list arrived.
    dropped: bool,
}

impl WinitState {
    fn dnd_offer(data_device: &WlDataDevice) -> Option<DragOffer> {
        data_device.data::<DataDeviceData>()?.drag_offer()
    }

    fn dnd_report_position(&mut self, window_id: WindowId, x: f64, y: f64) {
        let Some(window) = self.windows.get_mut().get(&window_id) else { return };
        let scale_factor = window.lock().unwrap().scale_factor();
        let position = LogicalPosition::new(x, y).to_physical(scale_factor);
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
        self.events_sink.push_window_event(WindowEvent::CursorMoved { device_id, position }, window_id);
    }

    /// The drag is over without a drop: stop reading, and take back the hover if it was shown.
    fn dnd_abort(&mut self) {
        let Some(dnd) = self.dnd.take() else { return };
        if let Some(token) = dnd.reader {
            self.loop_handle.remove(token);
        }
        if dnd.files.as_ref().is_some_and(|files| !files.is_empty()) {
            self.events_sink.push_window_event(WindowEvent::HoveredFileCancelled, dnd.window_id);
        }
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
        self.events_sink.push_window_event(WindowEvent::CursorLeft { device_id }, dnd.window_id);
        self.dispatched_events = true;
    }

    /// The whole list has arrived (or reading it failed): show the hover, or deliver the drop that
    /// was waiting for it.
    fn dnd_list_complete(&mut self) {
        let Some(dnd) = self.dnd.as_mut() else { return };
        dnd.reader = None;
        let files = parse_uri_list(&std::mem::take(&mut dnd.list));
        let window_id = dnd.window_id;
        let dropped = dnd.dropped;
        for file in &files {
            self.events_sink.push_window_event(WindowEvent::HoveredFile(file.clone()), window_id);
        }
        if let Some(dnd) = self.dnd.as_mut() {
            dnd.files = Some(files);
        }
        if dropped {
            self.dnd_deliver();
        }
        self.dispatched_events = true;
    }

    /// Hand the dropped files to the window and finish the transfer.
    fn dnd_deliver(&mut self) {
        let Some(dnd) = self.dnd.take() else { return };
        for file in dnd.files.unwrap_or_default() {
            self.events_sink.push_window_event(WindowEvent::DroppedFile(file), dnd.window_id);
        }
        if let Some(offer) = Self::dnd_offer(&dnd.data_device) {
            // `finish` is only valid for an offer whose action was settled on Copy.
            if offer.selected_action == DndAction::Copy {
                offer.finish();
            }
            offer.destroy();
        }
        self.dispatched_events = true;
    }

    /// Watch the pipe the list comes through.
    fn dnd_start_reading(&mut self, offer: &DragOffer) -> Option<RegistrationToken> {
        let pipe = offer
            .receive(URI_LIST.into())
            .map_err(|error| warn!("drag and drop: can't receive the file list: {error}"))
            .ok()?;
        let mut reader = pipe
            .as_fd()
            .try_clone_to_owned()
            .map(File::from)
            .map_err(|error| warn!("drag and drop: can't read the file list: {error}"))
            .ok()?;
        // Level-triggered: the callback runs while the pipe is readable, so one `read` never waits.
        self.loop_handle
            .insert_source(pipe, move |_, _, state: &mut WinitState| {
                let mut chunk = [0u8; 4096];
                match reader.read(&mut chunk) {
                    Ok(0) => {
                        state.dnd_list_complete();
                        PostAction::Remove
                    },
                    Ok(read) => {
                        let Some(dnd) = state.dnd.as_mut() else { return PostAction::Remove };
                        if dnd.list.len() + read > MAX_LIST_BYTES {
                            warn!("drag and drop: the file list is too long, ignoring it");
                            dnd.list.clear();
                            state.dnd_list_complete();
                            return PostAction::Remove;
                        }
                        dnd.list.extend_from_slice(&chunk[..read]);
                        PostAction::Continue
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => PostAction::Continue,
                    Err(error) => {
                        warn!("drag and drop: reading the file list failed: {error}");
                        if let Some(dnd) = state.dnd.as_mut() {
                            dnd.list.clear();
                        }
                        state.dnd_list_complete();
                        PostAction::Remove
                    },
                }
            })
            .map_err(|error| warn!("drag and drop: can't watch the file list: {error}"))
            .ok()
    }
}

impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        // A new drag replaces one that never ended.
        self.dnd_abort();

        let main_surface = surface.data::<SurfaceData>().and_then(|d| d.parent_surface()).unwrap_or(surface);
        let window_id = wayland::make_wid(main_surface);
        if !self.windows.get_mut().contains_key(&window_id) {
            return;
        }
        let Some(offer) = Self::dnd_offer(data_device) else { return };

        if !offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST)) {
            // Not files: say so, so the source shows that nothing can be dropped here.
            offer.accept_mime_type(offer.serial, None);
            offer.set_actions(DndAction::empty(), DndAction::empty());
            return;
        }
        offer.accept_mime_type(offer.serial, Some(URI_LIST.into()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);

        let reader = self.dnd_start_reading(&offer);
        self.dnd = Some(DndState {
            window_id,
            data_device: data_device.clone(),
            files: reader.is_none().then(Vec::new),
            reader,
            list: Vec::new(),
            dropped: false,
        });
        self.dnd_report_position(window_id, x, y);
        self.dispatched_events = true;
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        // After a drop the offer lives on until the list is read and handed over.
        if self.dnd.as_ref().is_some_and(|dnd| dnd.dropped) {
            return;
        }
        self.dnd_abort();
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        if let Some(window_id) = self.dnd.as_ref().map(|dnd| dnd.window_id) {
            self.dnd_report_position(window_id, x, y);
            self.dispatched_events = true;
        }
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(dnd) = self.dnd.as_mut() else { return };
        dnd.dropped = true;
        if dnd.files.is_some() {
            self.dnd_deliver();
        }
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

/// `delegate_data_device!` wants a source handler too. We never start a drag or offer a
/// selection, so none of these is ever called.
impl DataSourceHandler for WinitState {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

/// The local files in a `text/uri-list` (RFC 2483), in order. Comments, blank lines, other
/// schemes and `file:` URIs for another host are skipped; `%XX` escapes are decoded to bytes.
pub(crate) fn parse_uri_list(list: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    String::from_utf8_lossy(list)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|uri| {
            let rest = uri.strip_prefix("file:")?;
            // `file:///path` and `file://localhost/path` name a local file; `file:/path` does too.
            let path = match rest.strip_prefix("//") {
                Some(authority_and_path) => {
                    let (host, path) = authority_and_path.split_at(authority_and_path.find('/')?);
                    (host.is_empty() || host.eq_ignore_ascii_case("localhost")).then_some(path)?
                },
                None => rest.starts_with('/').then_some(rest)?,
            };
            let bytes = percent_decode(path.as_bytes());
            (!bytes.contains(&0)).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        })
        .collect()
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    fn digit(byte: u8) -> Option<u8> {
        char::from(byte).to_digit(16).and_then(|d| u8::try_from(d).ok())
    }
    let mut out = Vec::with_capacity(input.len());
    let mut rest = input;
    while let Some((&byte, tail)) = rest.split_first() {
        match (byte, tail) {
            (b'%', [high, low, after @ ..]) if digit(*high).is_some() && digit(*low).is_some() => {
                out.push(digit(*high).unwrap_or(0) << 4 | digit(*low).unwrap_or(0));
                rest = after;
            },
            _ => {
                out.push(byte);
                rest = tail;
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_files_are_listed_in_order_and_decoded() {
        let list = b"# a comment\r\nfile:///home/me/My%20clip.mp4\r\n\r\nfile://localhost/tmp/a.mov\r\nfile:/var/b.wav\n";
        assert_eq!(
            parse_uri_list(list),
            [PathBuf::from("/home/me/My clip.mp4"), PathBuf::from("/tmp/a.mov"), PathBuf::from("/var/b.wav")]
        );
    }

    #[test]
    fn other_hosts_and_schemes_are_skipped() {
        let list = b"https://example.com/a.mp4\nfile://nas.local/share/a.mp4\nftp://x/y\nfile:relative.mp4\n";
        assert!(parse_uri_list(list).is_empty());
    }

    #[test]
    fn escapes_are_decoded_to_bytes_and_bad_ones_are_kept() {
        // %D1%84 is UTF-8 for a Cyrillic letter; %FF is not UTF-8 and stays a raw byte.
        let paths = parse_uri_list("file:///t/%D1%84%25.mp4\nfile:///t/100%\nfile:///t/%zz%2\nfile:///t/%FF\n".as_bytes());
        assert_eq!(paths[0], PathBuf::from("/t/ф%.mp4"));
        assert_eq!(paths[1], PathBuf::from("/t/100%"));
        assert_eq!(paths[2], PathBuf::from("/t/%zz%2"));
        assert_eq!(paths[3].as_os_str().as_encoded_bytes(), b"/t/\xff");
    }

    #[test]
    fn a_nul_byte_in_a_path_is_refused() {
        assert!(parse_uri_list(b"file:///t/a%00b.mp4\n").is_empty());
    }
}

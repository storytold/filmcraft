//! USB Speed Editor service. HID I/O never blocks the UI. The worker owns its handle,
//! renews authentication, retries unplugged devices and exits when the hook is dropped.
//! Native Rust backends are enabled on Linux and Windows; no C HID library is shipped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_ui_egui::speed_editor::{Button, Event, Feedback};
use hidapi::{BusType, HidApi, HidDevice};

mod auth;

const VENDOR: u16 = 0x1edb;
const PRODUCT: u16 = 0xda0e;
const MAX_EVENTS: usize = 256;

#[derive(Default)]
struct Mailbox {
    events: VecDeque<Event>,
    feedback: Feedback,
    stop: bool,
}

struct Service {
    shared: Arc<Mutex<Mailbox>>,
    worker: std::thread::JoinHandle<()>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner).stop = true;
        // The poll has a 20 ms timeout; don't join a USB driver on the UI thread.
        self.worker.thread().unpark();
    }
}

type Poll = Box<dyn FnMut(Feedback) -> Vec<Event>>;

pub fn start(ctx: egui::Context) -> Result<Option<Poll>, String> {
    if std::env::var("FILMCRAFT_SPEED_EDITOR").is_ok_and(|v| matches!(v.as_str(), "0" | "off")) {
        return Ok(None);
    }
    let shared = Arc::new(Mutex::new(Mailbox::default()));
    let worker_shared = shared.clone();
    let worker = std::thread::Builder::new()
        .name("speed-editor".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&worker_shared, &ctx)));
            if result.is_err() {
                disconnected(&worker_shared, &ctx, "USB worker failed; restart FilmCraft to reconnect".into());
            }
        })
        .map_err(|e| format!("could not start USB worker: {e}"))?;
    let service = Service { shared, worker };
    Ok(Some(Box::new(move |feedback| {
        let mut mailbox = service.shared.lock().unwrap_or_else(PoisonError::into_inner);
        mailbox.feedback = feedback;
        mailbox.events.drain(..).collect()
    })))
}

fn stopped(shared: &Mutex<Mailbox>) -> bool {
    shared.lock().unwrap_or_else(PoisonError::into_inner).stop
}

fn publish(shared: &Mutex<Mailbox>, ctx: &egui::Context, event: Event) -> Result<(), String> {
    let mut mailbox = shared.lock().unwrap_or_else(PoisonError::into_inner);
    // Keep scrubbing responsive when frames render slowly. Combine adjacent relative
    // motion, retain only the latest absolute position, and never cross a key edge.
    if let Event::Wheel { mode, value } = &event
        && let Some(Event::Wheel { mode: previous_mode, value: previous_value }) = mailbox.events.back_mut()
        && mode == previous_mode
    {
        *previous_value = if matches!(mode, 0 | 2) { previous_value.saturating_add(*value) } else { *value };
        ctx.request_repaint();
        return Ok(());
    }
    if mailbox.events.len() >= MAX_EVENTS {
        return Err("USB input overflow; reconnecting to reset held buttons".into());
    }
    mailbox.events.push_back(event);
    ctx.request_repaint();
    Ok(())
}

fn disconnected(shared: &Mutex<Mailbox>, ctx: &egui::Context, detail: String) {
    let mut mailbox = shared.lock().unwrap_or_else(PoisonError::into_inner);
    // Never replay stale cuts or deletes after a disconnect or queue overflow.
    mailbox.events.clear();
    mailbox.events.push_back(Event::Connection { connected: false, detail });
    ctx.request_repaint();
}

fn run(shared: &Mutex<Mailbox>, ctx: &egui::Context) {
    let mut previous_error = String::new();
    while !stopped(shared) {
        let result = open().and_then(|(device, name)| serve(shared, ctx, device, name));
        if let Err(e) = result {
            if e != previous_error {
                log::warn!("Speed Editor: {e}");
            }
            disconnected(shared, ctx, e.clone());
            previous_error = e;
        } else {
            previous_error.clear();
        }
        if !stopped(shared) {
            std::thread::park_timeout(Duration::from_secs(2));
        }
    }
}

fn open() -> Result<(HidDevice, String), String> {
    let api = HidApi::new().map_err(|e| format!("could not enumerate USB devices: {e}"))?;
    let path = api
        .device_list()
        .find(|d| d.vendor_id() == VENDOR && d.product_id() == PRODUCT && matches!(d.bus_type(), BusType::Usb))
        .ok_or("No USB Speed Editor found; connect the board to retry automatically")?
        .path();
    let device = api
        .open_path(path)
        .map_err(|e| format!("Cannot open {}: {e}. On Linux grant your user read/write access with a hidraw udev rule", path.to_string_lossy()))?;
    let info = device.get_device_info().map_err(|e| format!("could not identify HID device: {e}"))?;
    if info.vendor_id() != VENDOR || info.product_id() != PRODUCT || !matches!(info.bus_type(), BusType::Usb) {
        return Err("discovered HID device is not a USB Blackmagic Design Speed Editor (1edb:da0e)".into());
    }
    Ok((device, path.to_string_lossy().into_owned()))
}

fn serve(shared: &Mutex<Mailbox>, ctx: &egui::Context, device: HidDevice, path: String) -> Result<(), String> {
    let mut renew = Instant::now() + auth::authenticate(&device)?;
    let mut feedback = shared.lock().unwrap_or_else(PoisonError::into_inner).feedback;
    // The first UI frame may not have published Jog yet.
    if feedback.wheel_leds == 0 {
        feedback.wheel_leds = 1;
    }
    configure(&device, None, feedback)?;
    publish(shared, ctx, Event::Connection { connected: true, detail: format!("Connected via USB ({path})") })?;
    let mut previous_buttons = Vec::new();
    while !stopped(shared) {
        if Instant::now() >= renew {
            renew = Instant::now() + auth::authenticate(&device)?;
        }
        let next = shared.lock().unwrap_or_else(PoisonError::into_inner).feedback;
        if next != feedback {
            configure(&device, Some(feedback), next)?;
            feedback = next;
        }
        let mut bytes = [0u8; 64];
        let n = device.read_timeout(&mut bytes, 20).map_err(|e| format!("USB read failed: {e}; reconnecting"))?;
        if n == 0 {
            continue;
        }
        let bytes = bytes.get(..n).ok_or("HID driver returned an oversized report")?;
        match decode(bytes) {
            Ok(Some(Event::Buttons(buttons))) => {
                if buttons != previous_buttons {
                    previous_buttons.clone_from(&buttons);
                    publish(shared, ctx, Event::TimedButtons { buttons, at: Instant::now() })?;
                }
            }
            Ok(Some(event)) => publish(shared, ctx, event)?,
            Ok(None) => {}
            Err(e) => return Err(format!("{e}; reconnecting")),
        }
    }
    // Best effort on shutdown: failures have no effect on the project or the UI.
    if let Err(e) = configure(&device, None, Feedback::default()) {
        log::debug!("Speed Editor shutdown: {e}");
    }
    Ok(())
}

fn write(device: &HidDevice, bytes: &[u8]) -> Result<(), String> {
    let n = device.write(bytes).map_err(|e| format!("USB output failed: {e}"))?;
    if n != bytes.len() {
        return Err("USB output report was only partially written".into());
    }
    Ok(())
}

fn configure(device: &HidDevice, previous: Option<Feedback>, next: Feedback) -> Result<(), String> {
    if previous.is_none_or(|p| p.wheel_mode != next.wheel_mode) {
        write(device, &[3, next.wheel_mode, 0, 0, 0, 0, 0])?;
    }
    if previous.is_none_or(|p| p.wheel_leds != next.wheel_leds) {
        write(device, &[4, next.wheel_leds])?;
    }
    if previous.is_none_or(|p| p.button_leds != next.button_leds) {
        let [a, b, c, d] = next.button_leds.to_le_bytes();
        write(device, &[2, a, b, c, d])?;
    }
    Ok(())
}

/// Fixed-size HID reports. Unknown report/key ids are ignored for firmware compatibility;
/// malformed known reports disconnect safely instead of leaving modifiers held.
fn decode(bytes: &[u8]) -> Result<Option<Event>, String> {
    // Native HID backends can pad short reports to the device's maximum report size.
    let expected = match bytes.first() {
        Some(3) => 7,
        Some(4) => 13,
        Some(7) => 3,
        _ => bytes.len(),
    };
    let bytes = match bytes.split_at_checked(expected) {
        Some((report, padding)) if padding.iter().all(|byte| *byte == 0) => report,
        _ => bytes,
    };
    match bytes {
        [3, mode @ (0..=3), a, b, c, d, _] => Ok(Some(Event::Wheel { mode: *mode, value: i32::from_le_bytes([*a, *b, *c, *d]) })),
        [4, keys @ ..] if keys.len() == 12 => {
            let mut buttons = Vec::with_capacity(6);
            for [a, b] in keys.as_chunks::<2>().0 {
                if let Some(button) = Button::from_code(u16::from_le_bytes([*a, *b]))
                    && !buttons.contains(&button)
                {
                    buttons.push(button);
                }
            }
            Ok(Some(Event::Buttons(buttons)))
        }
        [7, charging @ (0..=1), level @ (0..=100)] => Ok(Some(Event::Battery { charging: *charging == 1, level: *level })),
        [3 | 4 | 7, ..] | [] => Err("Malformed Speed Editor HID report".into()),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_decode_signed_motion_and_deduplicate_keys() {
        assert_eq!(decode(&[3, 0, 0, 255, 255, 255, 0]).unwrap(), Some(Event::Wheel { mode: 0, value: -256 }));
        assert_eq!(decode(&[4, 0x2f, 0, 0x2f, 0, 0xff, 0xff, 0, 0, 0, 0, 0, 0]).unwrap(), Some(Event::Buttons(vec![Button::Split])));
        assert_eq!(decode(&[7, 1, 80]).unwrap(), Some(Event::Battery { charging: true, level: 80 }));
    }

    #[test]
    fn known_reports_accept_zero_padding_and_reject_truncation_or_nonzero_padding() {
        for report in [vec![3, 0, 0, 255, 255, 255, 0], vec![4, 0x2f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], vec![7, 1, 80]] {
            let expected = decode(&report).unwrap();
            for n in 0..report.len() {
                assert!(decode(&report[..n]).is_err());
            }
            for len in report.len()..=64 {
                let mut padded = report.clone();
                padded.resize(len, 0);
                assert_eq!(decode(&padded).unwrap(), expected);
            }
            let mut padded = report.clone();
            padded.resize(64, 0);
            for i in report.len()..padded.len() {
                padded[i] = 1;
                assert!(decode(&padded).is_err());
                padded[i] = 0;
            }
        }
    }

    #[test]
    fn hostile_and_mutated_reports_never_panic() {
        let reports = [vec![3, 0, 0, 0, 0, 0, 0], vec![4; 13], vec![7, 0, 100]];
        for report in reports {
            for n in 0..report.len() {
                assert!(std::panic::catch_unwind(|| decode(&report[..n])).is_ok());
            }
            for i in 0..report.len() {
                for byte in 0..=255 {
                    let mut mutation = report.clone();
                    mutation[i] = byte;
                    assert!(std::panic::catch_unwind(|| decode(&mutation)).is_ok());
                }
            }
        }
        assert!(decode(&[3; 64]).is_err());
        assert!(decode(&[7, 1, 255]).is_err());
        assert!(decode(&[3, 255, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn overflow_and_disconnect_discard_stale_edits() {
        let shared = Mutex::new(Mailbox::default());
        let ctx = egui::Context::default();
        for _ in 0..MAX_EVENTS {
            publish(&shared, &ctx, Event::Buttons(vec![Button::Split])).unwrap();
        }
        assert!(publish(&shared, &ctx, Event::Buttons(vec![])).is_err());
        disconnected(&shared, &ctx, "Unplugged".into());
        let mailbox = shared.lock().unwrap();
        assert_eq!(mailbox.events.len(), 1);
        assert!(matches!(mailbox.events.front(), Some(Event::Connection { connected: false, .. })));
    }

    #[test]
    fn wheel_reports_coalesce_without_reordering_button_edges() {
        let shared = Mutex::new(Mailbox::default());
        let ctx = egui::Context::default();
        for value in [128, 256, -64] {
            publish(&shared, &ctx, Event::Wheel { mode: 0, value }).unwrap();
        }
        publish(&shared, &ctx, Event::Buttons(vec![Button::TrimIn])).unwrap();
        publish(&shared, &ctx, Event::Wheel { mode: 0, value: 128 }).unwrap();
        for value in [100, 200, 0] {
            publish(&shared, &ctx, Event::Wheel { mode: 3, value }).unwrap();
        }
        let events: Vec<_> = shared.lock().unwrap().events.drain(..).collect();
        assert_eq!(
            events,
            vec![
                Event::Wheel { mode: 0, value: 320 },
                Event::Buttons(vec![Button::TrimIn]),
                Event::Wheel { mode: 0, value: 128 },
                Event::Wheel { mode: 3, value: 0 }
            ]
        );
    }

    /// Deliberately opt-in: authenticates and briefly reads the real board. Does not edit a
    /// project or require a button press. Discovers the connected board by its USB IDs.
    #[test]
    #[ignore = "requires a connected USB Speed Editor"]
    fn usb_speed_editor_smoke() {
        let (device, path) = open().unwrap();
        let renew = auth::authenticate(&device).unwrap();
        assert!(renew >= Duration::from_secs(1));
        configure(&device, None, Feedback { wheel_mode: 0, wheel_leds: 1, button_leds: 0 }).unwrap();
        let mut reports = 0;
        for _ in 0..10 {
            let mut bytes = [0u8; 64];
            let n = device.read_timeout(&mut bytes, 20).unwrap();
            if n > 0 {
                decode(&bytes[..n]).unwrap();
                reports += 1;
            }
        }
        configure(&device, None, Feedback::default()).unwrap();
        println!("Authenticated {path}; renewal in {renew:?}; {reports} input reports decoded");
    }
}

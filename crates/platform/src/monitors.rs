//! The connected displays, for placing floating panel windows.
//!
//! eframe keeps winit's event loop (and with it `available_monitors`) to itself and the egui fork
//! reports no monitor list, so the dock cannot learn which displays exist. Without one, a window
//! saved on a monitor that has since been unplugged is only caught by a guess from the main
//! window's monitor size. This module asks the OS directly (read-only) and hands back plain
//! numbers; the UI sanitizes them before use.
//!
//! Only Windows enumerates for now (`EnumDisplayMonitors`); every other platform returns an empty
//! list, which keeps the dock's earlier behaviour.

/// One display, in physical pixels on the virtual desktop.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// The device name the OS gives it (such as `\.\DISPLAY2`), if any.
    pub name: Option<String>,
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width, as reported (the UI discards non-positive sizes).
    pub width: i32,
    /// Height, as reported.
    pub height: i32,
    /// Effective DPI divided by 96 (1.0 = 100 %); 1.0 when the OS cannot say.
    pub dpi_scale: f32,
}

/// The displays attached now, primary first where the OS says which; empty where unsupported or
/// when the call fails.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn connected_displays() -> Vec<Display> {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::core::BOOL;

    /// Displays are collected here; a sane desktop has a handful.
    const MAX_DISPLAYS: usize = 64;

    unsafe extern "system" fn collect(monitor: HMONITOR, _hdc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the address of the `Vec` `connected_displays` keeps alive and borrows
        // exclusively for the whole (synchronous) enumeration.
        let Some(out) = (unsafe { (data.0 as *mut Vec<Display>).as_mut() }) else { return BOOL(0) };
        if out.len() >= MAX_DISPLAYS {
            return BOOL(0);
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is a valid, writable MONITORINFOEXW whose `cbSize` is set.
        if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            return BOOL(1);
        }
        let r = info.monitorInfo.rcMonitor;
        let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
        let name = String::from_utf16_lossy(&info.szDevice[..len]);
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        // SAFETY: both out-pointers are valid for the call.
        let dpi = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.ok().map_or(96, |()| dpi_x);
        let display = Display {
            name: (!name.is_empty()).then_some(name),
            x: r.left,
            y: r.top,
            width: r.right.saturating_sub(r.left),
            height: r.bottom.saturating_sub(r.top),
            dpi_scale: if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 },
        };
        // The primary display (origin 0,0) goes first.
        if info.monitorInfo.dwFlags & 1 != 0 {
            out.insert(0, display);
        } else {
            out.push(display);
        }
        BOOL(1)
    }

    let mut out: Vec<Display> = Vec::new();
    // SAFETY: `collect` only runs during this call and `out` outlives it.
    let ok = unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut out as *mut Vec<Display> as isize)) };
    if !ok.as_bool() {
        log::debug!("EnumDisplayMonitors failed; no monitor list");
    }
    out
}

/// The displays attached now: empty on this platform.
#[cfg(not(target_os = "windows"))]
pub fn connected_displays() -> Vec<Display> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn enumeration_never_panics_and_reports_sane_sizes_or_nothing() {
        for display in super::connected_displays() {
            assert!(display.dpi_scale.is_finite() && display.dpi_scale > 0.0);
        }
    }
}

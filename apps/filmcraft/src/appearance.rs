//! The system's light or dark appearance for Settings ▸ Appearance ▸ Appearance Mode ▸ Sync with
//! system, on Linux desktops where winit reports none (Wayland compositors send no theme).
//!
//! The XDG desktop portal is read once (`org.freedesktop.portal.Settings.Read` of
//! `org.freedesktop.appearance` / `color-scheme`), then its `SettingChanged` signal is followed on a
//! worker thread, which wakes the UI only when the value changes. There is no polling. Without a
//! portal answer, `gsettings` runs once at start-up, by absolute path. Elsewhere the host returns no
//! service and the UI uses what egui reports.

#[cfg(target_os = "linux")]
const PORTAL_NAMESPACE: &str = "org.freedesktop.appearance";
#[cfg(target_os = "linux")]
const PORTAL_KEY: &str = "color-scheme";

#[cfg(target_os = "linux")]
fn portal_proxy() -> Option<zbus::blocking::Proxy<'static>> {
    let conn = zbus::blocking::connection::Builder::session().ok()?.method_timeout(std::time::Duration::from_secs(2)).build().ok()?;
    zbus::blocking::Proxy::new(&conn, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings").ok()
}

#[cfg(target_os = "linux")]
fn portal_theme(proxy: &zbus::blocking::Proxy<'_>) -> Option<egui::Theme> {
    let value: zbus::zvariant::OwnedValue = proxy.call("Read", &(PORTAL_NAMESPACE, PORTAL_KEY)).ok()?;
    decode(portal_code(value)?)
}

/// The `u32` inside a portal reply. `Read` returns a variant, and some portals nest another
/// variant inside it.
#[cfg(target_os = "linux")]
fn portal_code(value: zbus::zvariant::OwnedValue) -> Option<u32> {
    let mut value: zbus::zvariant::Value<'_> = value.into();
    for _ in 0..4 {
        match value {
            zbus::zvariant::Value::Value(inner) => value = *inner,
            other => return u32::try_from(other).ok(),
        }
    }
    None
}

/// The portal's `color-scheme`: 0 no preference, 1 prefer dark, 2 prefer light.
#[cfg(target_os = "linux")]
fn decode(code: u32) -> Option<egui::Theme> {
    match code {
        1 => Some(egui::Theme::Dark),
        2 => Some(egui::Theme::Light),
        _ => None,
    }
}

/// One-shot fallback for desktops without the settings portal: runs once, by absolute path.
#[cfg(target_os = "linux")]
fn gtk_theme() -> Option<egui::Theme> {
    let exe = ["/usr/bin/gsettings", "/bin/gsettings", "/usr/local/bin/gsettings"].into_iter().find(|p| std::path::Path::new(p).is_file())?;
    let output = std::process::Command::new(exe).args(["get", "org.gnome.desktop.interface", "color-scheme"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_gtk_scheme(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(target_os = "linux")]
fn parse_gtk_scheme(s: &str) -> Option<egui::Theme> {
    match s.trim().trim_matches('\'') {
        "prefer-dark" => Some(egui::Theme::Dark),
        "prefer-light" => Some(egui::Theme::Light),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn code_of(theme: Option<egui::Theme>) -> u8 {
    match theme {
        Some(egui::Theme::Dark) => 1,
        Some(egui::Theme::Light) => 2,
        None => 0,
    }
}

/// Publish initial and subsequent answers alike: the initial Read may finish after the UI's
/// bounded startup wait, so it must also wake an already-created context.
#[cfg(target_os = "linux")]
fn publish(value: &std::sync::atomic::AtomicU8, code: u8, repaint: impl FnOnce()) {
    if value.swap(code, std::sync::atomic::Ordering::Relaxed) != code {
        repaint();
    }
}

/// The host hook (`HostHooks::system_theme`), or `None` where egui's own report is used.
pub fn service() -> Option<filmcraft_ui_egui::SystemThemeFn> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::{Arc, OnceLock};
        let value = Arc::new(AtomicU8::new(0));
        let wake: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
        let (worker_value, worker_wake) = (Arc::clone(&value), Arc::clone(&wake));
        let (ready, wait) = std::sync::mpsc::channel();
        let worker = move || {
            let proxy = portal_proxy();
            // Subscribe before Read so a change during startup cannot be missed.
            let signals = proxy.as_ref().and_then(|p| p.receive_signal("SettingChanged").ok());
            let initial = proxy.as_ref().and_then(portal_theme).or_else(gtk_theme);
            publish(&worker_value, code_of(initial), || {
                if let Some(ctx) = worker_wake.get() {
                    ctx.request_repaint();
                }
            });
            let _ = ready.send(());
            let Some(signals) = signals else { return };
            // Sleeps in the signal iterator: no polling.
            for message in signals {
                let Ok((namespace, key, changed)) = message.body().deserialize::<(String, String, zbus::zvariant::OwnedValue)>() else {
                    continue;
                };
                if namespace != PORTAL_NAMESPACE || key != PORTAL_KEY {
                    continue;
                }
                let code = code_of(portal_code(changed).and_then(decode));
                publish(&worker_value, code, || {
                    if let Some(ctx) = worker_wake.get() {
                        ctx.request_repaint();
                    }
                });
            }
        };
        let spawned = std::thread::Builder::new().name("appearance-portal".into()).spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(worker)).is_err() {
                log::warn!("the system appearance watcher stopped; Sync with system keeps the last value");
            }
        });
        if spawned.is_err() {
            return None;
        }
        // The first reading, so the first frame already shows the right theme (bounded wait).
        let _ = wait.recv_timeout(std::time::Duration::from_millis(250));
        Some(Box::new(move |ctx: &egui::Context| {
            let _ = wake.set(ctx.clone());
            decode(u32::from(value.load(Ordering::Relaxed)))
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn initial_and_signal_answers_repaint_only_on_changes() {
        use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
        let repaints = AtomicUsize::new(0);
        let value = AtomicU8::new(0);
        let repaint = || {
            repaints.fetch_add(1, Ordering::Relaxed);
        };
        // A late initial Read takes the same publication path as later signals.
        super::publish(&value, 2, repaint);
        assert_eq!(repaints.load(Ordering::Relaxed), 1);
        super::publish(&value, 2, repaint);
        assert_eq!(repaints.load(Ordering::Relaxed), 1);
        super::publish(&value, 1, repaint);
        assert_eq!(repaints.load(Ordering::Relaxed), 2);
        assert_eq!(value.load(Ordering::Relaxed), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portal_and_gsettings_values_decode() {
        assert_eq!(super::decode(0), None);
        assert_eq!(super::decode(1), Some(egui::Theme::Dark));
        assert_eq!(super::decode(2), Some(egui::Theme::Light));
        assert_eq!(super::decode(9), None);
        let nested = zbus::zvariant::Value::Value(Box::new(zbus::zvariant::Value::Value(Box::new(zbus::zvariant::Value::U32(2)))));
        assert_eq!(super::portal_code(zbus::zvariant::OwnedValue::try_from(nested).unwrap()), Some(2));
        let plain = zbus::zvariant::OwnedValue::from(1u32);
        assert_eq!(super::portal_code(plain), Some(1));
        let wrong = zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from("dark")).unwrap();
        assert_eq!(super::portal_code(wrong), None);
        assert_eq!(super::parse_gtk_scheme("'prefer-light'\n"), Some(egui::Theme::Light));
        assert_eq!(super::parse_gtk_scheme("'prefer-dark'\n"), Some(egui::Theme::Dark));
        assert_eq!(super::parse_gtk_scheme("'default'\n"), None);
        assert_eq!(super::parse_gtk_scheme(""), None);
        assert_eq!((super::code_of(None), super::code_of(Some(egui::Theme::Dark)), super::code_of(Some(egui::Theme::Light))), (0, 1, 2));
    }
}

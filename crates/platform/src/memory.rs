//! Best-effort safe physical-RAM discovery. No dependency on a hardware codec succeeding.
use std::sync::Once;

pub(crate) fn configure() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Some(bytes) = physical_ram() {
            filmcraft_frame::memory::configure(bytes);
            log::info!("media memory budgets for {} MiB RAM: {:?}", bytes >> 20, filmcraft_frame::memory::budgets());
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Err(err) = std::thread::Builder::new().name("media-memory".into()).spawn(|| {
            let result = std::panic::catch_unwind(|| {
                let mut pressure = filmcraft_frame::memory::Pressure::Normal;
                loop {
                    if let Some(free) = available_percent() {
                        let next = classify(free, pressure);
                        if next != pressure {
                            filmcraft_frame::memory::set_pressure(next);
                            log::info!("media memory pressure: {next:?} ({free}% available)");
                            pressure = next;
                        }
                        // Retry as formerly recent caches become idle during paused playback.
                        if next != filmcraft_frame::memory::Pressure::Normal {
                            filmcraft_codecs::gop::trim_memory();
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(15));
                }
            });
            if result.is_err() {
                log::error!("media memory monitor stopped after a panic");
            }
        }) {
            log::warn!("cannot start media memory monitor: {err}");
        }
    });
}

#[cfg(target_os = "macos")]
fn physical_ram() -> Option<u64> {
    let out = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    std::str::from_utf8(&out.stdout).ok()?.trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn physical_ram() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok()?.checked_mul(1024)
}

#[cfg(target_os = "windows")]
fn physical_ram() -> Option<u64> {
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    std::str::from_utf8(&out.stdout).ok()?.trim().parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn physical_ram() -> Option<u64> {
    None
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn classify(free: u8, previous: filmcraft_frame::memory::Pressure) -> filmcraft_frame::memory::Pressure {
    use filmcraft_frame::memory::Pressure::*;
    if free < 10 {
        Critical
    } else if free < 20 || (free < 25 && previous != Normal) {
        Warning
    } else {
        Normal
    }
}

#[cfg(target_os = "macos")]
fn available_percent() -> Option<u8> {
    // A bounded, best-effort query on a dedicated thread: neither a stuck OS utility nor
    // missing permissions can block playback. This is an available-memory heuristic.
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/bin/memory_pressure").arg("-Q").stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(2) => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_percent(std::str::from_utf8(&out.stdout).ok()?)
}

#[cfg(any(target_os = "macos", test))]
fn parse_percent(text: &str) -> Option<u8> {
    let line = text.lines().find(|line| line.starts_with("System-wide memory free percentage:"))?;
    let value = line.split(':').nth(1)?.trim().strip_suffix('%')?.trim().parse::<u8>().ok()?;
    (value <= 100).then_some(value)
}

#[cfg(target_os = "linux")]
fn available_percent() -> Option<u8> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| text.lines().find(|line| line.starts_with(name))?.split_whitespace().nth(1)?.parse::<u64>().ok();
    let total = field("MemTotal:")?;
    let free = field("MemAvailable:")?;
    if total == 0 {
        return None;
    }
    Some((free.saturating_mul(100) / total).min(100) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_frame::memory::Pressure::*;
    #[test]
    fn query_parser_and_pressure_hysteresis() {
        assert_eq!(parse_percent("The system has 8589934592.\nSystem-wide memory free percentage: 39%\n"), Some(39));
        assert_eq!(parse_percent("System-wide memory free percentage: 200%"), None);
        assert_eq!(parse_percent("garbage"), None);
        assert_eq!(classify(9, Normal), Critical);
        assert_eq!(classify(19, Critical), Warning);
        assert_eq!(classify(23, Warning), Warning);
        assert_eq!(classify(23, Normal), Normal);
        assert_eq!(classify(25, Warning), Normal);
    }
}

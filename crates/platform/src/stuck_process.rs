//! Ending a FilmCraft start that hung in the graphics driver (#1061).
//!
//! A start whose graphics driver hangs while being loaded can't end itself: on Windows neither
//! starting a process nor `abort` gets past the hang (most likely the loader lock the driver
//! holds). The app's start-up watchdog leaves a marker, and the next launch ends the stuck process
//! with [`end_stuck_process`] from outside, which works.
//!
//! Like `cursor`, this is a non-media OS call: `crates/platform` is the only crate allowed `unsafe`
//! (AGENTS.md §0.3, ADR 0001), so the few process calls live here behind a safe `Result` API. It
//! only ends a process that is still the same program, started when the marker says, so a process
//! id that was reused (by another program, or by a FilmCraft that started fine) is left alone.
//! Elsewhere there is nothing to end: the hangs it is for were seen on Windows.

/// How far a process's creation may be from the launch time a marker records: the marker is
/// written a moment after the process started, by the process itself.
pub const LAUNCH_TOLERANCE_SECS: u64 = 60;

/// Whether a process created at `created` (Unix seconds) is the one that recorded `launched`: it
/// started before it recorded its launch, and not long before.
pub fn same_launch(created: u64, launched: u64) -> bool {
    created <= launched.saturating_add(2) && launched.saturating_sub(created) <= LAUNCH_TOLERANCE_SECS
}

/// End process `pid` if it is still a process of the executable named `exe_name` (file name only,
/// any case) that was started at `launched` (Unix seconds, see [`same_launch`]). `Ok(true)` when
/// it was ended; `Ok(false)` when there is nothing to end (gone, another program, a later process
/// with the same id, or this process); `Err` when the OS would not tell or not end it.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn end_stuck_process(pid: u32, exe_name: &str, launched: u64) -> Result<bool, String> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
    };
    use windows::core::PWSTR;

    /// Closes the process handle however this returns.
    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from a successful `OpenProcess` and is closed only here, once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    if pid == 0 || pid == std::process::id() || exe_name.is_empty() {
        return Ok(false);
    }
    // SAFETY: a plain call with no pointers; the handle it returns is owned by `Owned`.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        // gone, or not ours to open
        return Ok(false);
    };
    let process = Owned(handle);
    let mut path = [0u16; 1024];
    let mut len = u32::try_from(path.len()).unwrap_or(0);
    // SAFETY: `path` is writable for `len` UTF-16 units for the duration of the call, and `len`
    // is a valid, writable u32 that the call sets to the units written.
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(path.as_mut_ptr()), &mut len) }.map_err(|e| format!("process {pid}: {e}"))?;
    let path = String::from_utf16_lossy(path.get(..usize::try_from(len).unwrap_or(0)).unwrap_or(&[]));
    let name = path.rsplit(['\\', '/']).next().unwrap_or("");
    if !name.eq_ignore_ascii_case(exe_name) {
        return Ok(false);
    }
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the four FILETIMEs are valid and writable for the duration of the call.
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }.map_err(|e| format!("process {pid}: {e}"))?;
    if !same_launch(unix_secs(created.dwHighDateTime, created.dwLowDateTime), launched) {
        return Ok(false);
    }
    // SAFETY: `process.0` was opened with PROCESS_TERMINATE and is still open.
    unsafe { TerminateProcess(process.0, 1) }.map_err(|e| format!("process {pid}: {e}"))?;
    Ok(true)
}

/// Elsewhere there is nothing to end (see the module docs).
#[cfg(not(target_os = "windows"))]
pub fn end_stuck_process(_pid: u32, _exe_name: &str, _launched: u64) -> Result<bool, String> {
    Ok(false)
}

/// A Windows FILETIME (100-nanosecond ticks since 1601) in Unix seconds; 0 before 1970.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn unix_secs(high: u32, low: u32) -> u64 {
    const TICKS_PER_SEC: u64 = 10_000_000;
    const SECS_1601_TO_1970: u64 = 11_644_473_600;
    ((u64::from(high) << 32 | u64::from(low)) / TICKS_PER_SEC).saturating_sub(SECS_1601_TO_1970)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_matches_only_a_process_created_just_before_it() {
        assert!(same_launch(1000, 1000));
        assert!(same_launch(1000, 1005));
        assert!(same_launch(1001, 1000), "clocks a second apart");
        assert!(same_launch(1000 - LAUNCH_TOLERANCE_SECS, 1000));
        // a process started later reused the id; one started long before is not that launch
        assert!(!same_launch(1010, 1000));
        assert!(!same_launch(1000 - LAUNCH_TOLERANCE_SECS - 1, 1000));
        // hostile times don't overflow
        assert!(!same_launch(u64::MAX, 0));
        assert!(same_launch(0, 0));
    }

    #[test]
    fn filetimes_convert_to_unix_seconds() {
        // 1970-01-01 and 2026-10-14 00:00 UTC
        let at = |unix: u64| {
            let ticks = (unix + 11_644_473_600) * 10_000_000;
            unix_secs((ticks >> 32) as u32, ticks as u32)
        };
        assert_eq!(at(0), 0);
        assert_eq!(at(1_791_936_000), 1_791_936_000);
        assert_eq!(unix_secs(0, 0), 0, "before 1970");
        assert_eq!(unix_secs(u32::MAX, u32::MAX), u64::MAX / 10_000_000 - 11_644_473_600);
    }

    #[test]
    fn nothing_is_ended_that_is_not_the_stuck_start() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        // this process, no process, no name
        assert_eq!(end_stuck_process(std::process::id(), "anything.exe", now), Ok(false));
        assert_eq!(end_stuck_process(0, "anything.exe", now), Ok(false));
        assert_eq!(end_stuck_process(u32::MAX - 3, "anything.exe", now), Ok(false));
        assert_eq!(end_stuck_process(1, "", now), Ok(false));
    }

    /// A real process: left alone under another name or another launch time, ended when both match.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_matching_process_is_ended() {
        let mut child = std::process::Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let pid = child.id();
        assert_eq!(end_stuck_process(pid, "filmcraft.exe", now), Ok(false), "another program");
        assert_eq!(end_stuck_process(pid, "PING.EXE", now - 3600), Ok(false), "another launch");
        assert_eq!(child.try_wait().unwrap(), None, "still running");
        assert_eq!(end_stuck_process(pid, "PING.EXE", now), Ok(true));
        assert!(child.wait().is_ok_and(|s| !s.success()));
    }
}

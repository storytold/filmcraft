//! Host system memory detection.

/// Query the host machine's total physical memory in bytes, if available.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn total_physical_memory() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `mem` is properly initialized with dwLength set.
    unsafe { GlobalMemoryStatusEx(&mut mem) }.ok()?;
    (mem.ullTotalPhys > 0).then_some(mem.ullTotalPhys)
}

#[cfg(target_os = "linux")]
pub fn total_physical_memory() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn total_physical_memory() -> Option<u64> {
    None
}

/// Compute a recommended GOP frame cache budget (in bytes) scaled to total physical RAM.
///
/// Default base budget is 1 GiB (`filmcraft_codecs::FRAME_BUDGET`).
/// On systems with >= 32 GiB RAM, scaled conservatively to 2.5 GiB.
/// On systems with >= 16 GiB RAM, scaled to 1.5 GiB.
/// On 32-bit/WASM targets or systems with < 16 GiB, kept at 1.0 GiB.
pub fn default_frame_budget_for_ram(total_bytes: u64) -> usize {
    let gib = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let budget_bytes: u64 = if gib >= 30.0 {
        // 2.5 GiB on 32+ GiB systems
        5 * (512 * 1024 * 1024)
    } else if gib >= 14.0 {
        // 1.5 GiB on 16+ GiB systems
        3 * (512 * 1024 * 1024)
    } else {
        // 1.0 GiB base default
        1024 * 1024 * 1024
    };
    budget_bytes.min(usize::MAX as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_scaled_budget_calculations() {
        // < 14 GiB -> 1 GiB
        assert_eq!(default_frame_budget_for_ram(8 * 1024 * 1024 * 1024), 1 << 30);
        // 16 GiB -> 1.5 GiB
        assert_eq!(default_frame_budget_for_ram(16 * 1024 * 1024 * 1024), 3 << 29);
        // 32 GiB -> 2.5 GiB
        assert_eq!(default_frame_budget_for_ram(32 * 1024 * 1024 * 1024), (5u64 << 29).min(usize::MAX as u64) as usize);
        // 64 GiB -> 2.5 GiB (maintainer conservative ceiling)
        assert_eq!(default_frame_budget_for_ram(64 * 1024 * 1024 * 1024), (5u64 << 29).min(usize::MAX as u64) as usize);
    }
}

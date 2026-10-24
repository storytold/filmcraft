use crate::{ExportError, Result};

/// Build guard only: stop at 83%, emergency stop at 85%.
const BUILD_START_LIMIT_PERCENT: u64 = 83;
const BUILD_EMERGENCY_LIMIT_PERCENT: u64 = 85;

/// Runtime RAM ceiling for exports: stop new work at 90% host RAM use, same reading as the build
/// guard but a different threshold and a different caller (export admission/backpressure, not the
/// build helper). The build guard keeps its 83/85% constants and is not reused for runtime.
pub const RUNTIME_CEILING_PERCENT: u64 = 90;

/// Additional slots that fit a snapshot of *uncommitted* host/heap headroom, in bytes.
/// Count shared UMA capacity with the minimum, never the sum. Persistent codec allocations and
/// outstanding reservations must include every buffer/surface and must not already be subtracted
/// from these headroom values. The queue must separately cap total live transfer leases at three.
/// Callers retain reservations until retirement; this calculation never releases resources.
pub(crate) fn admissible_slots(
    host_headroom: Option<u64>,
    heap_headroom: Option<u64>,
    persistent_bytes: u64,
    outstanding_bytes: u64,
    slot_bytes: u64,
) -> usize {
    let Some(host) = host_headroom else { return 0 };
    let capacity = host.min(heap_headroom.unwrap_or(host / 2));
    let Some(reserved) = persistent_bytes.checked_add(outstanding_bytes) else { return 0 };
    let Some(remaining) = capacity.checked_sub(reserved) else { return 0 };
    if slot_bytes == 0 || remaining == 0 {
        return 0;
    }
    // Leave one byte below the ceiling, including when a slot would otherwise fit exactly.
    ((remaining - 1) / slot_bytes).min(3) as usize
}

fn host_total_kib() -> Option<u64> {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else { return None };
    let mut total_kib = None;
    for line in meminfo.lines() {
        let mut fields = line.split_whitespace();
        if let Some("MemTotal:") = fields.next() {
            total_kib = fields.next().and_then(|v| v.parse::<u64>().ok());
        }
    }
    total_kib.filter(|&t| t > 0)
}

fn host_available_kib() -> Option<u64> {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else { return None };
    let mut available_kib = None;
    for line in meminfo.lines() {
        let mut fields = line.split_whitespace();
        if let Some("MemAvailable:") = fields.next() {
            available_kib = fields.next().and_then(|v| v.parse::<u64>().ok());
        }
    }
    available_kib
}

/// Best-effort current host RAM use as a percent of total (Linux only); `None` when the reading is
/// unavailable or nonsensical. Used by both the build guard and the export runtime ceiling.
fn host_ram_percent() -> Option<u64> {
    let (Some(total), Some(available)) = (host_total_kib(), host_available_kib()) else { return None };
    ram_use_percent(total, available)
}

fn ram_use_percent(total: u64, available: u64) -> Option<u64> {
    if total == 0 || available > total {
        return None;
    }
    Some(total.saturating_sub(available).saturating_mul(100) / total)
}

/// Runtime admission check for exports: refuse further work when host RAM use is at or above 90%.
/// Unsupported hosts and unreadable meminfo are treated as "cannot prove pressure", so they do not
/// gate exports; a reading that is missing or nonsensical is not treated as extra headroom.
pub fn check_runtime_ram_ceiling() -> Result<()> {
    check_runtime_ram_ceiling_at(host_ram_percent())
}

fn check_runtime_ram_ceiling_at(percent: Option<u64>) -> Result<()> {
    let Some(percent) = percent else { return Ok(()) };
    if percent >= RUNTIME_CEILING_PERCENT {
        return Err(ExportError::MemoryPressure(format!(
            "host RAM use is {percent}% (runtime ceiling {RUNTIME_CEILING_PERCENT}%); export stopped before more work"
        )));
    }
    Ok(())
}

/// Runtime headroom in KiB for exports: how much MemAvailable is left before the 90% ceiling, or
/// None when the host reading is unavailable/unsane. A missing reading is not a headroom estimate.
pub fn runtime_ram_headroom_kib() -> Option<u64> {
    let (Some(total_kib), Some(available_kib)) = (host_total_kib(), host_available_kib()) else { return None };
    runtime_ram_headroom_at(total_kib, available_kib)
}

fn runtime_ram_headroom_at(total_kib: u64, available_kib: u64) -> Option<u64> {
    if total_kib == 0 || available_kib > total_kib {
        return None;
    }
    let ceiling_kib = (u128::from(total_kib) * u128::from(100 - RUNTIME_CEILING_PERCENT) / 100) as u64;
    if available_kib >= ceiling_kib {
        return Some(available_kib.saturating_sub(ceiling_kib));
    }
    Some(0)
}

/// Build guard only: stop at 83%, emergency stop at 85%.
pub(super) fn check() -> Result<()> {
    let Some(percent) = host_ram_percent() else { return Ok(()) };
    if percent >= BUILD_START_LIMIT_PERCENT {
        let threshold = if percent >= BUILD_EMERGENCY_LIMIT_PERCENT { BUILD_EMERGENCY_LIMIT_PERCENT } else { BUILD_START_LIMIT_PERCENT };
        return Err(ExportError::MemoryPressure(format!("host RAM use is {percent}% (build limit {threshold}%); build stopped before more work")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_headroom_measures_distance_to_ninety_percent() {
        assert_eq!(super::runtime_ram_headroom_at(1_000, 110), Some(10));
        assert_eq!(super::runtime_ram_headroom_at(1_000, 100), Some(0));
        assert_eq!(super::runtime_ram_headroom_at(1_000, 90), Some(0));
        assert_eq!(super::runtime_ram_headroom_at(0, 0), None);
        assert_eq!(super::runtime_ram_headroom_at(100, 101), None);
        assert_eq!(super::runtime_ram_headroom_at(u64::MAX, u64::MAX), Some(16_602_069_666_338_596_454));
    }

    #[test]
    fn slot_budget_respects_host_heap_and_three_slot_cap() {
        // Persistent codec memory is separate from per-transfer-slot reservations.
        assert_eq!(super::admissible_slots(Some(1_000), Some(900), 100, 0, 200), 3);
        assert_eq!(super::admissible_slots(Some(1_000), Some(600), 100, 0, 200), 2);
        assert_eq!(super::admissible_slots(Some(600), Some(1_000), 100, 0, 200), 2);
        assert_eq!(super::admissible_slots(Some(500), Some(500), 100, 0, 200), 1);
        assert_eq!(super::admissible_slots(Some(300), Some(300), 100, 0, 200), 0);
    }

    #[test]
    fn slot_budget_keeps_outstanding_reservations_and_fails_closed() {
        assert_eq!(super::admissible_slots(Some(1_000), Some(1_000), 100, 400, 200), 2);
        assert_eq!(super::admissible_slots(Some(1_000), None, 100, 0, 200), 1);
        assert_eq!(super::admissible_slots(None, Some(1_000), 0, 0, 200), 0);
        assert_eq!(super::admissible_slots(Some(1_000), Some(1_000), 0, 0, 0), 0);
        assert_eq!(super::admissible_slots(Some(1_000), Some(1_000), u64::MAX, 1, 200), 0);
        assert_eq!(super::admissible_slots(Some(100), Some(100), 101, 0, 1), 0);
        assert_eq!(super::admissible_slots(Some(u64::MAX), Some(u64::MAX), 0, 0, 1), 3);
    }

    #[test]
    fn runtime_threshold_is_independent_of_build_guard() {
        assert!(super::check_runtime_ram_ceiling_at(Some(89)).is_ok());
        assert!(super::check_runtime_ram_ceiling_at(Some(90)).is_err());
        assert!(super::check_runtime_ram_ceiling_at(None).is_ok());
        assert_eq!(super::ram_use_percent(100, 10), Some(90));
        assert_eq!(super::ram_use_percent(100, 101), None);
    }
}

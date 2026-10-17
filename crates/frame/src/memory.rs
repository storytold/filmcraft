//! Shared memory policy for decoded frames, recyclable buffers, GPU uploads and export overlap.
//! Platform startup supplies physical RAM; unknown hosts keep the historical ceilings.
//! These are cache budgets, not a cap on indispensable frames currently being displayed.
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

static RAM: AtomicU64 = AtomicU64::new(0);
static PRESSURE: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    Warning,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    pub decoded: usize,
    pub per_clip: usize,
    pub plane_shelf: usize,
    pub float_shelf: usize,
    pub gpu_uploads: usize,
    pub export_overlap: usize,
}

/// Pure policy, also used by tests. At 8 GiB RAM cache ceilings total 1.125 GiB;
/// separate active GPU working images, codec state and UI allocations still need headroom.
pub fn for_ram(bytes: u64, pressure: Pressure) -> Budgets {
    let ram = usize::try_from(bytes).unwrap_or(usize::MAX);
    let cap = |old: usize, divisor: usize| if bytes == 0 { old } else { old.min(ram / divisor) };
    let divisor = match pressure {
        Pressure::Normal => 1,
        Pressure::Warning => 2,
        Pressure::Critical => 8,
    };
    Budgets {
        decoded: cap(1 << 30, 16) / divisor,
        per_clip: cap(384 << 20, 64) / divisor,
        plane_shelf: cap(192 << 20, 128) / divisor,
        float_shelf: cap(320 << 20, 64) / divisor,
        gpu_uploads: cap(512 << 20, 32) / divisor,
        export_overlap: cap(512 << 20, 64) / divisor,
    }
}

pub fn budgets() -> Budgets {
    let pressure = match PRESSURE.load(Ordering::Relaxed) {
        1 => Pressure::Warning,
        2 => Pressure::Critical,
        _ => Pressure::Normal,
    };
    for_ram(RAM.load(Ordering::Relaxed), pressure)
}

/// Called by the platform at startup (zero leaves unknown-host defaults).
pub fn configure(bytes: u64) {
    RAM.store(bytes, Ordering::Relaxed);
    crate::pool::trim_to_budget();
}

/// Hosts may supply memory-pressure notifications without making low-level crates depend on OS APIs.
/// Idle pools release excess allocations immediately; live caches trim on their next operation.
pub fn set_pressure(pressure: Pressure) {
    PRESSURE.store(
        match pressure {
            Pressure::Normal => 0,
            Pressure::Warning => 1,
            Pressure::Critical => 2,
        },
        Ordering::Relaxed,
    );
    crate::pool::trim_to_budget();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn low_ram_and_pressure_reduce_every_cache_without_overflow() {
        let normal = for_ram(8 << 30, Pressure::Normal);
        assert_eq!(normal.decoded, 512 << 20);
        assert_eq!(normal.per_clip, 128 << 20);
        assert_eq!(normal.plane_shelf, 64 << 20);
        assert_eq!(normal.float_shelf, 128 << 20);
        assert_eq!(normal.gpu_uploads, 256 << 20);
        assert_eq!(normal.export_overlap, 128 << 20);
        assert_eq!(for_ram(8 << 30, Pressure::Critical).decoded, 64 << 20);
        assert_eq!(for_ram(0, Pressure::Normal).decoded, 1 << 30);
        assert!(for_ram(u64::MAX, Pressure::Normal).decoded <= 1 << 30);
        assert_eq!(for_ram(1, Pressure::Critical).decoded, 0);
    }
}

//! One CPU transfer per GPU picture; platform adapters own execution and resource retention.

use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::PixelData;

#[derive(Default)]
pub(crate) struct CpuState {
    pub(crate) footprint: crate::memory::Footprint,
    result: OnceLock<Result<PixelData, String>>,
    started: Mutex<bool>,
    done: Condvar,
}

/// Current CPU representation; pending work is neither pixels nor a terminal error.
pub enum CpuReadiness<'a> {
    Pending,
    Ready(&'a PixelData),
    Failed(&'a str),
}

/// Exactly one completion permit. Dropping unfinished work records a terminal error.
pub struct CpuTransfer(Arc<CpuState>);

impl CpuTransfer {
    pub fn complete(self, result: Result<PixelData, String>) {
        self.finish(result);
    }

    fn finish(&self, result: Result<PixelData, String>) {
        let result = result.and_then(|pixels| match pixels {
            PixelData::Gpu(_) => Err("GPU download returned another GPU surface".into()),
            pixels => Ok(pixels),
        });
        let _guard = self.0.started.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.result.get().is_none() {
            if let Ok(pixels) = &result {
                self.0.footprint.add_cpu(pixels.byte_size());
            }
            let _ = self.0.result.set(result);
        }
        self.0.done.notify_all();
    }
}

impl Drop for CpuTransfer {
    fn drop(&mut self) {
        if self.0.result.get().is_none() {
            self.finish(Err("CPU transfer abandoned".into()));
        }
    }
}

impl CpuState {
    pub(crate) fn new(gpu_bytes: usize) -> Self {
        Self { footprint: crate::memory::Footprint::new(gpu_bytes), ..Self::default() }
    }

    pub(crate) fn start(self: &Arc<Self>) -> Option<CpuTransfer> {
        let mut started = self.started.lock().unwrap_or_else(|e| e.into_inner());
        if *started {
            return None;
        }
        *started = true;
        Some(CpuTransfer(self.clone()))
    }

    pub(crate) fn readiness(&self) -> CpuReadiness<'_> {
        match self.result.get() {
            None => CpuReadiness::Pending,
            Some(Ok(pixels)) => CpuReadiness::Ready(pixels),
            Some(Err(error)) => CpuReadiness::Failed(error),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wait(&self) {
        let mut guard = self.started.lock().unwrap_or_else(|e| e.into_inner());
        while self.result.get().is_none() {
            guard = self.done.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_is_not_cached_as_failure_and_clones_share_one_transfer() {
        let state = Arc::new(CpuState::default());
        let transfer = state.start().unwrap();
        assert!(state.clone().start().is_none());
        assert!(matches!(state.readiness(), CpuReadiness::Pending));
        transfer.complete(Ok(PixelData::Rgba8(Arc::new(vec![]))));
        assert!(matches!(state.readiness(), CpuReadiness::Ready(PixelData::Rgba8(_))));
    }

    #[test]
    fn abandoned_transfer_is_terminal() {
        let state = Arc::new(CpuState::default());
        drop(state.start().unwrap());
        assert!(matches!(state.readiness(), CpuReadiness::Failed("CPU transfer abandoned")));
        assert!(state.start().is_none());
    }

    #[test]
    fn failed_and_abandoned_transfers_do_not_charge_cpu_payload() {
        let gpu_bytes = 2 * 2 * 4; // Fixture2x2 RGBA8 surface.
        for abandon in [false, true] {
            let state = Arc::new(CpuState::new(gpu_bytes));
            let account = crate::MemoryAccount::default();
            let charge = state.footprint.charge(&account);
            let transfer = state.start().unwrap();
            if abandon {
                drop(transfer);
            } else {
                transfer.complete(Err("injected readback failure".into()));
            }
            assert!(matches!(state.readiness(), CpuReadiness::Failed(_)));
            assert_eq!(account.bytes(), gpu_bytes, "failed extraction retains only the fixture GPU surface");
            drop(charge);
            assert_eq!(account.bytes(), 0);
        }
    }

    #[test]
    fn delayed_success_updates_charges_before_ready_is_published() {
        let gpu_bytes = 2 * 2 * 4;
        let payload = vec![0.0; 2 * 2 * 4];
        let cpu_bytes = payload.len() * size_of::<f32>();
        let state = Arc::new(CpuState::new(gpu_bytes));
        let account = crate::MemoryAccount::default();
        let charge = state.footprint.charge(&account);
        let transfer = state.start().unwrap();
        assert!(matches!(state.readiness(), CpuReadiness::Pending));
        assert_eq!(account.bytes(), gpu_bytes);
        transfer.complete(Ok(PixelData::RgbaF32(Arc::new(payload))));
        assert!(matches!(state.readiness(), CpuReadiness::Ready(_)));
        assert_eq!(account.bytes(), gpu_bytes + cpu_bytes);
        drop(charge);
        assert_eq!(account.bytes(), 0);
    }

    #[test]
    fn failure_is_shared_and_not_retried() {
        let state = Arc::new(CpuState::default());
        state.start().unwrap().complete(Err("injected readback failure".into()));
        for copy in [state.clone(), state] {
            assert!(matches!(copy.readiness(), CpuReadiness::Failed("injected readback failure")));
            assert!(copy.start().is_none());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_waiter_observes_completion() {
        let state = Arc::new(CpuState::default());
        let transfer = state.start().unwrap();
        let other = state.clone();
        let waiter = std::thread::spawn(move || {
            other.wait();
            assert!(matches!(other.readiness(), CpuReadiness::Ready(_)));
        });
        transfer.complete(Ok(PixelData::Rgba8(Arc::new(vec![]))));
        waiter.join().unwrap();
    }
}

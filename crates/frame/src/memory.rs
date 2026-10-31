//! Shared payload estimates +cache-lifetime charges; completion never calls cache code.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// Estimated payload held by one cache, including late CPU materialization.
#[derive(Clone, Default)]
pub struct MemoryAccount(Arc<Mutex<u128>>);

impl MemoryAccount {
    /// Current estimate; saturates the public view without losing later subtraction precision.
    pub fn bytes(&self) -> usize {
        usize::try_from(*self.0.lock().unwrap_or_else(PoisonError::into_inner)).unwrap_or(usize::MAX)
    }

    fn replace(&self, old: usize, new: usize) {
        let mut bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *bytes = bytes.saturating_sub(old as u128).saturating_add(new as u128);
    }

    /// Charge fixed payload until the returned lease is dropped.
    pub fn charge(&self, bytes: usize) -> MemoryLease {
        self.replace(0, bytes);
        MemoryLease(vec![Arc::new(Charge { account: self.clone(), bytes: AtomicUsize::new(bytes) })])
    }
}

/// Cache entry ownership, independent of picture clones outside that cache.
#[derive(Default)]
pub struct MemoryLease(Vec<Arc<Charge>>);

impl MemoryLease {
    /// Combine source and prepared-payload charges into one entry lifetime.
    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }
}

struct Charge {
    account: MemoryAccount,
    bytes: AtomicUsize,
}

impl Charge {
    fn set(&self, bytes: usize) {
        self.account.replace(self.bytes.swap(bytes, Ordering::Relaxed), bytes);
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.account.replace(self.bytes.load(Ordering::Relaxed), 0);
    }
}

#[derive(Default)]
pub(crate) struct Footprint(Mutex<State>);

#[derive(Default)]
struct State {
    bytes: usize,
    charges: Vec<Weak<Charge>>,
}

impl Footprint {
    pub(crate) fn new(bytes: usize) -> Self {
        Self(Mutex::new(State { bytes, charges: Vec::new() }))
    }

    pub(crate) fn bytes(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).bytes
    }

    pub(crate) fn charge(&self, account: &MemoryAccount) -> MemoryLease {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.charges.retain(|charge| charge.strong_count() != 0);
        let lease = account.charge(state.bytes);
        state.charges.extend(lease.0.iter().map(Arc::downgrade));
        lease
    }

    pub(crate) fn add_cpu(&self, bytes: usize) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.bytes = state.bytes.saturating_add(bytes);
        let total = state.bytes;
        state.charges.retain(|charge| {
            if let Some(charge) = charge.upgrade() {
                charge.set(total);
                true
            } else {
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPU_BYTES: usize = 4 * 4 * 3 / 2; // Fixture4x4 NV12.
    const CPU_BYTES: usize = 4 * 4 * 4 * size_of::<f32>(); // Same picture as RGBAf32.
    const TOTAL: usize = GPU_BYTES + CPU_BYTES;

    #[test]
    fn late_payload_updates_every_live_charge_and_drop_restores_totals() {
        let footprint = Footprint::new(GPU_BYTES);
        let (a, b) = (MemoryAccount::default(), MemoryAccount::default());
        let first = footprint.charge(&a);
        let replaced = footprint.charge(&a);
        let second = footprint.charge(&b);
        assert_eq!((a.bytes(), b.bytes()), (GPU_BYTES * 2, GPU_BYTES));
        drop(replaced);
        footprint.add_cpu(CPU_BYTES);
        // 4x4 NV12 surface24 +4x4 RGBAf32 payload256, once per retained reference.
        assert_eq!((a.bytes(), b.bytes()), (TOTAL, TOTAL));
        let after = footprint.charge(&a);
        assert_eq!(a.bytes(), TOTAL * 2);
        drop((first, after, second));
        assert_eq!((a.bytes(), b.bytes()), (0, 0));
    }

    #[test]
    fn saturated_public_view_preserves_exact_drop_accounting() {
        let account = MemoryAccount::default();
        let a = account.charge(usize::MAX);
        let b = account.charge(usize::MAX);
        assert_eq!(account.bytes(), usize::MAX);
        drop(a);
        assert_eq!(account.bytes(), usize::MAX);
        drop(b);
        assert_eq!(account.bytes(), 0);
    }

    #[test]
    fn concurrent_registration_and_completion_have_one_final_charge() {
        let footprint = Arc::new(Footprint::new(GPU_BYTES));
        let account = MemoryAccount::default();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let other = footprint.clone();
        let gate = barrier.clone();
        let worker = std::thread::spawn(move || {
            gate.wait();
            other.add_cpu(CPU_BYTES);
        });
        barrier.wait();
        let charge = footprint.charge(&account);
        worker.join().unwrap();
        assert_eq!(account.bytes(), TOTAL);
        drop(charge);
        assert_eq!(account.bytes(), 0);
    }

    #[test]
    fn completion_racing_last_cache_reference_cannot_leave_a_charge() {
        let footprint = Arc::new(Footprint::new(GPU_BYTES));
        let account = MemoryAccount::default();
        let charge = footprint.charge(&account);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let gate = barrier.clone();
        let other = footprint.clone();
        let worker = std::thread::spawn(move || {
            gate.wait();
            other.add_cpu(CPU_BYTES);
        });
        barrier.wait();
        drop(charge);
        worker.join().unwrap();
        assert_eq!(account.bytes(), 0);
    }
}

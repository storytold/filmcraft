//! Resident AMD export: the per-frame reservation ledger, the bounded handoff queue, and the
//! traits a host implements to admit a GPU-resident encoder session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use filmcraft_isobmff::SampleEntry;
use filmcraft_project::{ItemId, Project};
use filmcraft_render::{SourceProvider, plan::PreflightedFrame};
use filmcraft_time::Tick;

use crate::ram_guard;
use crate::settings;
use crate::{EncodedPacket, ExportError, ExportSettings, Format, Result};

struct ResidentLedger {
    slots: usize,
    bytes: u64,
}

fn resident_ledger() -> &'static Mutex<ResidentLedger> {
    static LEDGER: OnceLock<Mutex<ResidentLedger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(ResidentLedger { slots: 0, bytes: 0 }))
}

struct ResidentFrameState {
    total_bytes: u64,
    status: Mutex<ResidentFrameStatus>,
}

struct ResidentFrameStatus {
    remaining_bytes: u64,
    allocations: usize,
    parent_released: bool,
    ledger_released: bool,
}

impl ResidentFrameState {
    fn release_ledger_if_idle(&self, status: &mut ResidentFrameStatus) {
        if status.parent_released && status.allocations == 0 && !status.ledger_released {
            status.ledger_released = true;
            let mut ledger = resident_ledger().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger.slots = ledger.slots.saturating_sub(1);
            ledger.bytes = ledger.bytes.saturating_sub(self.total_bytes);
        }
    }
}

/// One frame's global three-slot and byte reservation. Keep it alive through the frame callback;
/// `take` divides its fixed byte budget into move-only allocation holds. Dropping allocation holds
/// never replenishes this frame's budget, preventing repeated allocations from reusing bytes.
pub struct ResidentReservation {
    state: Arc<ResidentFrameState>,
}

impl ResidentReservation {
    /// Reserve the entire resident frame before any platform or GPU allocation. Headroom inputs
    /// exclude `persistent_bytes` and other caller-owned reservations; global outstanding frames
    /// are subtracted internally. Unknown host headroom denies work. Unknown heap headroom uses
    /// half of host headroom as a conservative shared-UMA allowance.
    pub fn try_acquire(host_headroom_bytes: Option<u64>, heap_headroom_bytes: Option<u64>, persistent_bytes: u64, frame_bytes: u64) -> Result<Self> {
        let Some(host) = host_headroom_bytes else {
            return Err(ExportError::MemoryPressure("resident frame admission needs current host RAM headroom".into()));
        };
        if frame_bytes == 0 {
            return Err(ExportError::MemoryPressure("resident frame reservation must be nonzero".into()));
        }
        let mut ledger = resident_ledger().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if ledger.slots >= 3 || ram_guard::admissible_slots(Some(host), heap_headroom_bytes, persistent_bytes, ledger.bytes, frame_bytes) == 0 {
            return Err(ExportError::MemoryPressure("resident export is at its three-slot or RAM/heap budget limit".into()));
        }
        let Some(bytes) = ledger.bytes.checked_add(frame_bytes) else {
            return Err(ExportError::MemoryPressure("resident frame byte reservation overflow".into()));
        };
        ledger.slots += 1;
        ledger.bytes = bytes;
        Ok(Self {
            state: Arc::new(ResidentFrameState {
                total_bytes: frame_bytes,
                status: Mutex::new(ResidentFrameStatus { remaining_bytes: frame_bytes, allocations: 0, parent_released: false, ledger_released: false }),
            }),
        })
    }

    pub fn bytes(&self) -> u64 {
        self.state.total_bytes
    }

    pub fn remaining_bytes(&self) -> u64 {
        self.state.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remaining_bytes
    }

    /// Take a one-use allocation hold before allocating native/GPU memory. Dropping the hold
    /// releases it only after the caller's owning resource drops; its byte allowance stays spent.
    pub fn take(&mut self, bytes: u64) -> Result<ResidentAllocation> {
        if bytes == 0 {
            return Err(ExportError::MemoryPressure("resident allocation reservation must be nonzero".into()));
        }
        let mut status = self.state.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(remaining_bytes) = status.remaining_bytes.checked_sub(bytes) else {
            return Err(ExportError::MemoryPressure("resident frame allocation exceeds its reserved byte budget".into()));
        };
        let Some(allocations) = status.allocations.checked_add(1) else {
            return Err(ExportError::MemoryPressure("resident allocation hold count overflow".into()));
        };
        status.remaining_bytes = remaining_bytes;
        status.allocations = allocations;
        Ok(ResidentAllocation { state: self.state.clone(), bytes })
    }
}

impl Drop for ResidentReservation {
    fn drop(&mut self) {
        let mut status = self.state.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        status.parent_released = true;
        self.state.release_ledger_if_idle(&mut status);
    }
}

/// Move-only byte hold to store beside the exact allocation it budgets.
pub struct ResidentAllocation {
    state: Arc<ResidentFrameState>,
    bytes: u64,
}

impl ResidentAllocation {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for ResidentAllocation {
    fn drop(&mut self) {
        let mut status = self.state.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        status.allocations = status.allocations.saturating_sub(1);
        self.state.release_ledger_if_idle(&mut status);
    }
}

struct ResidentEnvelope<T> {
    index: u64,
    pts: Tick,
    payload: T,
    reservation: ResidentReservation,
}

/// Error that returns both payload and reservation, so failed enqueue cannot lose frame ownership.
pub struct ResidentSendError<T> {
    pub error: ExportError,
    pub payload: T,
    pub reservation: ResidentReservation,
}

pub struct ResidentProducer<T: Send> {
    tx: mpsc::SyncSender<ResidentEnvelope<T>>,
    cancelled: Arc<AtomicBool>,
    last_index: Option<u64>,
}

pub struct ResidentConsumer<T: Send> {
    rx: mpsc::Receiver<ResidentEnvelope<T>>,
    pending: Option<ResidentEnvelope<T>>,
    failed: bool,
    cancelled: Arc<AtomicBool>,
}

/// Bounded handoff: at most three frames across queued envelopes, active callbacks, and allocation
/// holds. Acquire `ResidentReservation` before import or allocation. Submit work only inside
/// `process_next`; pre-submitted payloads must quarantine their resource and allocation hold on drop.
pub fn resident_queue<T: Send>() -> (ResidentProducer<T>, ResidentConsumer<T>) {
    let (tx, rx) = mpsc::sync_channel(3);
    let cancelled = Arc::new(AtomicBool::new(false));
    (ResidentProducer { tx, cancelled: cancelled.clone(), last_index: None }, ResidentConsumer { rx, pending: None, failed: false, cancelled })
}

impl<T: Send> ResidentProducer<T> {
    /// Enqueue a reserved, not-yet-submitted frame. Frame indexes must be consecutive and increasing.
    /// Queued payloads drop normally when the receiver closes. Pre-submitted work must quarantine
    /// its resource and allocation hold through retirement.
    pub fn send_reserved(
        &mut self,
        frame_index: u64,
        pts: Tick,
        reservation: ResidentReservation,
        payload: T,
    ) -> std::result::Result<(), ResidentSendError<T>> {
        let error = if self.cancelled.load(Ordering::Acquire) {
            Some(ExportError::Cancelled)
        } else if self.last_index.is_some_and(|last| last.checked_add(1) != Some(frame_index)) {
            Some(ExportError::Encode("resident queue requires consecutive frame indexes".into()))
        } else {
            None
        };
        if let Some(error) = error {
            return Err(ResidentSendError { error, payload, reservation });
        }
        match self.tx.send(ResidentEnvelope { index: frame_index, pts, payload, reservation }) {
            Ok(()) => {
                self.last_index = Some(frame_index);
                Ok(())
            }
            Err(error) => Err(ResidentSendError { error: ExportError::Cancelled, payload: error.0.payload, reservation: error.0.reservation }),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl<T: Send> ResidentConsumer<T> {
    /// Poll oldest frame. Return `Ok` only after the platform and GPU consumers have retired it.
    /// Failure or panic stops admission and retains pending frame plus reservation; no retry occurs.
    pub fn process_next<E>(
        &mut self,
        process: impl FnOnce(u64, Tick, &mut T, &mut ResidentReservation) -> std::result::Result<(), E>,
    ) -> std::result::Result<bool, E> {
        if self.failed || self.cancelled.load(Ordering::Acquire) {
            return Ok(false);
        }
        if self.pending.is_none() {
            self.pending = match self.rx.try_recv() {
                Ok(envelope) => Some(envelope),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return Ok(false),
            };
        }
        let processed = {
            let Some(envelope) = self.pending.as_mut() else { return Ok(false) };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(envelope.index, envelope.pts, &mut envelope.payload, &mut envelope.reservation)))
        };
        let processed = match processed {
            Ok(processed) => processed,
            Err(panic) => {
                self.failed = true;
                self.cancelled.store(true, Ordering::Release);
                std::panic::resume_unwind(panic);
            }
        };
        if let Err(error) = processed {
            self.failed = true;
            self.cancelled.store(true, Ordering::Release);
            return Err(error);
        }
        drop(self.pending.take());
        Ok(true)
    }
}

impl<T: Send> Drop for ResidentConsumer<T> {
    fn drop(&mut self) {
        if let Some(envelope) = self.pending.take() {
            // One process-lifetime leak per uncertain pending frame; global ledger caps total at three.
            // Callback may have submitted work before failing or unwinding; retain payload and budget.
            std::mem::forget(envelope);
        }
    }
}

/// Capability admission owned by one export job; implementations must not open encoder sessions.
pub trait AmdExportAdmission: Send + Sync {
    fn validate_settings(&self, settings: &ExportSettings, resolved: &settings::Resolved) -> Result<()>;

    fn create_session(
        &self,
        _project: Arc<Project>,
        _seq: ItemId,
        _settings: &ExportSettings,
        _resolved: &settings::Resolved,
    ) -> Result<Box<dyn ResidentVideoSession>> {
        Err(ExportError::Unsupported("AMD resident video session is unavailable".into()))
    }
}

/// One serial, GPU-resident video encoder session owned by an export job.
pub trait ResidentVideoSession: Send {
    fn sample_entry(&self) -> SampleEntry;
    fn timescale(&self) -> u32;
    fn media_start(&self) -> Option<i64> {
        None
    }
    /// Checked aggregate bytes needed for this frame, including native and output allocations.
    fn frame_bytes(&self, frame: &PreflightedFrame) -> Result<u64>;
    fn encode_frame(
        &mut self,
        index: u64,
        frame: PreflightedFrame,
        sources: &dyn SourceProvider,
        reservation: &mut ResidentReservation,
    ) -> Result<Vec<EncodedPacket>>;
    fn flush(&mut self) -> Result<Vec<EncodedPacket>>;
}

/// Host provider bound to the exact renderer adapter for this engine session.
pub trait AmdExportProvider: Send + Sync {
    fn admit(&self, format: Format) -> Result<Arc<dyn AmdExportAdmission>>;
}

#[cfg(test)]
pub(crate) mod resident_reservation_tests {
    use super::{ResidentReservation, resident_queue};
    use crate::Tick;
    use std::sync::{Mutex, OnceLock};

    pub(crate) fn lock_ledger() -> std::sync::MutexGuard<'static, ()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ledger_snapshot() -> (usize, u64) {
        let ledger = super::resident_ledger().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (ledger.slots, ledger.bytes)
    }

    fn restore_ledger_for_test(snapshot: (usize, u64)) {
        let mut ledger = super::resident_ledger().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (ledger.slots, ledger.bytes) = snapshot;
    }

    struct RestoreLedgerOnDrop((usize, u64));

    impl Drop for RestoreLedgerOnDrop {
        fn drop(&mut self) {
            restore_ledger_for_test(self.0);
        }
    }

    struct DropSentinel(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for DropSentinel {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }

    #[test]
    fn frame_budget_is_single_use_and_slot_lives_with_child_allocation() {
        let _guard = lock_ledger();
        assert!(ResidentReservation::try_acquire(None, Some(1_000), 0, 10).is_err());
        assert!(ResidentReservation::try_acquire(Some(1_000), None, 0, 501).is_err());
        assert!(ResidentReservation::try_acquire(Some(1_000), Some(1_000), 901, 100).is_err());
        let held = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(ResidentReservation::try_acquire(Some(150), Some(150), 0, 100).is_err(), "new lower headroom must shrink admission");
        drop(held);

        let (mut producer, mut consumer) = resident_queue();
        let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(producer.send_reserved(0, Tick::ZERO, reservation, String::from("frame")).is_ok());
        let mut imported_copy = None;
        assert!(
            consumer
                .process_next(|index, pts, payload, budget| {
                    assert_eq!((index, pts, payload.as_str()), (0, Tick::ZERO, "frame"));
                    imported_copy = Some(budget.take(40)?);
                    assert_eq!(budget.remaining_bytes(), 60);
                    assert!(budget.take(61).is_err());
                    Ok::<(), crate::ExportError>(())
                })
                .is_ok()
        );

        let allocation = imported_copy.expect("allocation hold returned from callback");
        let second = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("second slot");
        let third = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("third slot");
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err());
        drop((second, third, allocation));
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_ok());
    }

    #[test]
    fn concurrent_parent_and_last_child_drop_release_one_ledger_charge() {
        let _guard = lock_ledger();
        let baseline = ledger_snapshot();
        let _restore = RestoreLedgerOnDrop(baseline);

        for _ in 0..128 {
            let mut reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 1).expect("budget fits");
            let allocation = reservation.take(1).expect("child allocation fits");
            let gate = std::sync::Arc::new(std::sync::Barrier::new(3));
            let parent_gate = gate.clone();
            let child_gate = gate.clone();
            let parent = std::thread::spawn(move || {
                parent_gate.wait();
                drop(reservation);
            });
            let child = std::thread::spawn(move || {
                child_gate.wait();
                drop(allocation);
            });
            gate.wait();
            parent.join().expect("parent drop thread");
            child.join().expect("allocation drop thread");
            assert_eq!(ledger_snapshot(), baseline, "concurrent final drops must release exactly once");
        }
    }

    #[test]
    fn failed_retirement_quarantines_and_unsubmitted_cancellation_releases() {
        let _guard = lock_ledger();
        let baseline = ledger_snapshot();
        let _restore = RestoreLedgerOnDrop(baseline);
        let (mut producer, mut consumer) = resident_queue();
        let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(producer.send_reserved(0, Tick::ZERO, reservation, ()).is_ok());
        assert!(consumer.process_next::<&str>(|_, _, _, _| Err("retirement failed")).is_err());
        assert!(!consumer.process_next(|_, _, _, _| Ok::<(), ()>(())).unwrap_or(false));

        let second = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("second slot");
        let third = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("third slot");
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err());
        drop(consumer);
        drop((second, third));
        assert_eq!(ledger_snapshot(), (baseline.0 + 1, baseline.1 + 100), "consumer drop must keep failed work charged");
        let slot_a = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("second slot remains available");
        let slot_b = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("third slot remains available");
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err(), "quarantine must retain one of three slots");
        drop((slot_a, slot_b));
        restore_ledger_for_test(baseline);
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_ok());

        let (mut producer, mut consumer) = resident_queue::<()>();
        let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(producer.send_reserved(0, Tick::ZERO, reservation, ()).is_ok());
        producer.cancel();
        let mut called = false;
        assert!(
            !consumer
                .process_next(|_, _, _, _| {
                    called = true;
                    Ok::<(), ()>(())
                })
                .unwrap_or(false)
        );
        assert!(!called);
        let second = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("second slot");
        let third = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("third slot");
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err());
        drop(consumer);
        drop((second, third));
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_ok());
    }

    #[test]
    fn failed_cancelled_pending_payload_and_budget_survive_consumer_drop() {
        let _guard = lock_ledger();
        let baseline = ledger_snapshot();
        let _restore = RestoreLedgerOnDrop(baseline);
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut producer, mut consumer) = resident_queue();
        let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(producer.send_reserved(0, Tick::ZERO, reservation, DropSentinel(dropped.clone())).is_ok());
        assert!(
            consumer
                .process_next(|_, _, _, _| {
                    producer.cancel();
                    Err::<(), _>("submitted but retirement not confirmed")
                })
                .is_err()
        );
        drop(consumer);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0, "uncertain payload must stay quarantined");
        assert!(ResidentReservation::try_acquire(Some(150), Some(150), 0, 100).is_err(), "quarantined bytes must remain charged");
        let second = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("second slot");
        let third = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("third slot");
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err(), "quarantine must retain one of three slots");
        drop((second, third));
        assert_eq!(ledger_snapshot(), (baseline.0 + 1, baseline.1 + 100));
        restore_ledger_for_test(baseline);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0, "pending payload remains retained");
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_ok());
    }
    #[test]
    fn successful_retirement_drops_payload_and_releases_reservation() {
        let _guard = lock_ledger();
        let baseline = ledger_snapshot();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut producer, mut consumer) = resident_queue();
        let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(producer.send_reserved(0, Tick::ZERO, reservation, DropSentinel(dropped.clone())).is_ok());
        assert!(consumer.process_next(|_, _, _, _| Ok::<(), ()>(())).unwrap_or(false));
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(ledger_snapshot(), baseline, "confirmed retirement releases aggregate reservation");
    }

    #[test]
    fn unwinding_callback_is_terminal_and_keeps_pending_payload_and_budget() {
        let _guard = lock_ledger();
        let baseline = ledger_snapshot();
        let _restore = RestoreLedgerOnDrop(baseline);
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut producer, mut consumer) = resident_queue();
        let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("budget fits");
        assert!(producer.send_reserved(0, Tick::ZERO, reservation, DropSentinel(dropped.clone())).is_ok());
        let mut calls = 0;
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = consumer.process_next(|_, _, _, _| -> std::result::Result<(), ()> {
                calls += 1;
                panic!("simulated panic after submit")
            });
        }));
        assert!(unwind.is_err());
        assert_eq!(calls, 1);
        assert!(
            !consumer
                .process_next(|_, _, _, _| {
                    calls += 1;
                    Ok::<(), ()>(())
                })
                .unwrap_or(false)
        );
        assert_eq!(calls, 1, "caught panic must not retry uncertain work");
        assert!(matches!(
            producer.send_reserved(
                1,
                Tick::ZERO,
                ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("another slot remains available"),
                DropSentinel(std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0))),
            ),
            Err(error) if matches!(error.error, super::ExportError::Cancelled)
        ));
        drop(consumer);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(ledger_snapshot(), (baseline.0 + 1, baseline.1 + 100));
        restore_ledger_for_test(baseline);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn resident_queue_bounds_in_flight_to_three() {
        let _guard = lock_ledger();
        let baseline = ledger_snapshot();
        let _restore = RestoreLedgerOnDrop(baseline);
        let (mut producer, mut consumer) = resident_queue::<u32>();
        for index in 0..3u64 {
            let reservation = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("slot fits");
            assert!(producer.send_reserved(index, Tick::ZERO, reservation, index as u32).is_ok());
        }
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err(), "a fourth frame waits while three are in flight");
        assert_eq!(ledger_snapshot().0, baseline.0 + 3);
        assert!(consumer.process_next(|_, _, _, _| Ok::<(), ()>(())).unwrap_or(false));
        assert_eq!(ledger_snapshot().0, baseline.0 + 2, "retiring the oldest frame frees its slot");
        let fourth = ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).expect("fourth frame fits after retirement");
        assert!(producer.send_reserved(3, Tick::ZERO, fourth, 3).is_ok());
        assert!(ResidentReservation::try_acquire(Some(10_000), Some(10_000), 0, 100).is_err(), "still three in flight");
        drop(producer);
        drop(consumer);
        assert_eq!(ledger_snapshot(), baseline, "queued frames release their slots when the queue closes");
    }
}

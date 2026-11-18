//! Process-wide counters for CPU ↔ GPU pixel transfers across decode, compositing and encode.
//!
//! Subtract two [`snapshot`] readings to measure one benchmark run or playback interval.

use std::ops::Sub;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cumulative CPU ↔ GPU transfer operations and byte counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferStats {
    /// Hardware video decoder readback (GPU → CPU).
    pub decode_readback_ops: u64,
    pub decode_readback_bytes: u64,
    /// Compositor texture upload (CPU → GPU).
    pub compositor_upload_ops: u64,
    pub compositor_upload_bytes: u64,
    /// Export compositor readback (GPU → CPU).
    pub export_readback_ops: u64,
    pub export_readback_bytes: u64,
    /// Hardware video encoder input upload (CPU → GPU).
    pub encode_upload_ops: u64,
    pub encode_upload_bytes: u64,
}

impl TransferStats {
    pub fn total_ops(self) -> u64 {
        self.decode_readback_ops + self.compositor_upload_ops + self.export_readback_ops + self.encode_upload_ops
    }

    pub fn total_bytes(self) -> u64 {
        self.decode_readback_bytes + self.compositor_upload_bytes + self.export_readback_bytes + self.encode_upload_bytes
    }

    pub fn preview_ops(self) -> u64 {
        self.decode_readback_ops + self.compositor_upload_ops
    }

    pub fn preview_bytes(self) -> u64 {
        self.decode_readback_bytes + self.compositor_upload_bytes
    }

    pub fn export_ops(self) -> u64 {
        self.decode_readback_ops + self.compositor_upload_ops + self.export_readback_ops + self.encode_upload_ops
    }

    pub fn export_bytes(self) -> u64 {
        self.decode_readback_bytes + self.compositor_upload_bytes + self.export_readback_bytes + self.encode_upload_bytes
    }
}

impl Sub for TransferStats {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            decode_readback_ops: self.decode_readback_ops.saturating_sub(rhs.decode_readback_ops),
            decode_readback_bytes: self.decode_readback_bytes.saturating_sub(rhs.decode_readback_bytes),
            compositor_upload_ops: self.compositor_upload_ops.saturating_sub(rhs.compositor_upload_ops),
            compositor_upload_bytes: self.compositor_upload_bytes.saturating_sub(rhs.compositor_upload_bytes),
            export_readback_ops: self.export_readback_ops.saturating_sub(rhs.export_readback_ops),
            export_readback_bytes: self.export_readback_bytes.saturating_sub(rhs.export_readback_bytes),
            encode_upload_ops: self.encode_upload_ops.saturating_sub(rhs.encode_upload_ops),
            encode_upload_bytes: self.encode_upload_bytes.saturating_sub(rhs.encode_upload_bytes),
        }
    }
}

static DECODE_READBACK_OPS: AtomicU64 = AtomicU64::new(0);
static DECODE_READBACK_BYTES: AtomicU64 = AtomicU64::new(0);
static COMPOSITOR_UPLOAD_OPS: AtomicU64 = AtomicU64::new(0);
static COMPOSITOR_UPLOAD_BYTES: AtomicU64 = AtomicU64::new(0);
static EXPORT_READBACK_OPS: AtomicU64 = AtomicU64::new(0);
static EXPORT_READBACK_BYTES: AtomicU64 = AtomicU64::new(0);
static ENCODE_UPLOAD_OPS: AtomicU64 = AtomicU64::new(0);
static ENCODE_UPLOAD_BYTES: AtomicU64 = AtomicU64::new(0);

pub fn record_decode_readback(bytes: u64) {
    DECODE_READBACK_OPS.fetch_add(1, Ordering::Relaxed);
    DECODE_READBACK_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn record_compositor_upload(bytes: u64) {
    COMPOSITOR_UPLOAD_OPS.fetch_add(1, Ordering::Relaxed);
    COMPOSITOR_UPLOAD_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn record_export_readback(bytes: u64) {
    EXPORT_READBACK_OPS.fetch_add(1, Ordering::Relaxed);
    EXPORT_READBACK_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn record_encode_upload(bytes: u64) {
    ENCODE_UPLOAD_OPS.fetch_add(1, Ordering::Relaxed);
    ENCODE_UPLOAD_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub fn snapshot() -> TransferStats {
    TransferStats {
        decode_readback_ops: DECODE_READBACK_OPS.load(Ordering::Relaxed),
        decode_readback_bytes: DECODE_READBACK_BYTES.load(Ordering::Relaxed),
        compositor_upload_ops: COMPOSITOR_UPLOAD_OPS.load(Ordering::Relaxed),
        compositor_upload_bytes: COMPOSITOR_UPLOAD_BYTES.load(Ordering::Relaxed),
        export_readback_ops: EXPORT_READBACK_OPS.load(Ordering::Relaxed),
        export_readback_bytes: EXPORT_READBACK_BYTES.load(Ordering::Relaxed),
        encode_upload_ops: ENCODE_UPLOAD_OPS.load(Ordering::Relaxed),
        encode_upload_bytes: ENCODE_UPLOAD_BYTES.load(Ordering::Relaxed),
    }
}

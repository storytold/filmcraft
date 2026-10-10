//! Download spacing. It is pure, so it runs natively even though the rest of the crate is
//! wasm32-only.

#[path = "../src/download_pacing.rs"]
mod download_pacing;

use download_pacing::{GAP_MS, Pacer};

/// #378: 20 downloads requested in the same instant are spread out, not clicked in one burst.
#[test]
fn burst_of_downloads_is_spaced_out() {
    let mut p = Pacer::default();
    let delays: Vec<f64> = (0..20).map(|_| p.reserve(1000.0)).collect();
    assert_eq!(delays[0], 0.0);
    for (i, d) in delays.iter().enumerate() {
        assert_eq!(*d, i as f64 * GAP_MS);
    }
}

/// A download after a quiet period is not delayed.
#[test]
fn download_after_a_pause_is_immediate() {
    let mut p = Pacer::default();
    assert_eq!(p.reserve(0.0), 0.0);
    assert_eq!(p.reserve(10_000.0), 0.0);
}

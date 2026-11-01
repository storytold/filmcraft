//! The Layer III hybrid analysis filterbank: the polyphase analysis subband filter of
//! ISO/IEC 11172-3 Annex C.1.3 (32 subbands), then per subband an 18-line MDCT with the normal
//! (block type 0) window over two granules, and the encoder side of the alias-reduction
//! butterflies (the inverse of §2.4.3.4.10.4's decoder butterflies).

use std::sync::OnceLock;

use crate::tables::{alias_coefficients, analysis_matrix, analysis_window};

/// Frequency lines per granule.
pub(crate) const GRANULE: usize = 576;

/// The polyphase analysis filter of one channel: a 512-sample FIFO, newest sample first.
#[derive(Clone)]
pub(crate) struct Polyphase {
    x: [f32; 512],
}

impl Polyphase {
    pub fn new() -> Self {
        Polyphase { x: [0.0; 512] }
    }

    /// Shift in 32 samples (oldest first) and compute the 32 subband samples (Annex C.1.3).
    pub fn filter(&mut self, input: &[f32; 32], out: &mut [f32; 32]) {
        self.x.copy_within(0..480, 32);
        for (i, s) in input.iter().enumerate() {
            // X[31] receives the first of the new samples, X[0] the last
            self.x[31 - i] = *s;
        }
        let c = analysis_window();
        let mut y = [0f32; 64];
        for (i, yi) in y.iter_mut().enumerate() {
            let mut acc = 0f32;
            for j in 0..8 {
                acc += c[i + 64 * j] * self.x[i + 64 * j];
            }
            *yi = acc;
        }
        for (o, row) in out.iter_mut().zip(analysis_matrix()) {
            *o = row.iter().zip(&y).map(|(m, y)| m * y).sum();
        }
    }
}

/// The MDCT kernel for normal blocks: `w[i] cos(π/72 (2i + 1 + 18)(2k + 1)) / 9`, `k` in `0..18`,
/// `i` in `0..36`, with the sine window `w[i] = sin(π/36 (i + 1/2))`. The 1/9 (= 2/18) makes it the
/// exact inverse of the decoder's unscaled IMDCT with windowed overlap-add.
fn mdct_kernel() -> &'static [[f32; 36]; 18] {
    static K: OnceLock<[[f32; 36]; 18]> = OnceLock::new();
    K.get_or_init(|| {
        let mut k = [[0f32; 36]; 18];
        for (kk, row) in k.iter_mut().enumerate() {
            for (i, c) in row.iter_mut().enumerate() {
                let w = (std::f64::consts::PI / 36.0 * (i as f64 + 0.5)).sin();
                let a = std::f64::consts::PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * kk + 1) as f64;
                *c = (w * a.cos() / 9.0) as f32;
            }
        }
        k
    })
}

/// The hybrid filterbank of one channel: the polyphase filter and the previous granule's subband
/// samples for the MDCT overlap.
#[derive(Clone)]
pub(crate) struct Analysis {
    poly: Polyphase,
    prev: [[f32; 18]; 32],
}

impl Analysis {
    pub fn new() -> Self {
        Analysis { poly: Polyphase::new(), prev: [[0.0; 18]; 32] }
    }

    /// Transform one granule of 576 samples into 576 frequency lines (`xr[18 * sb + k]`).
    pub fn granule(&mut self, pcm: &[f32; GRANULE], xr: &mut [f32; GRANULE]) {
        let mut cur = [[0f32; 18]; 32];
        let mut slot = [0f32; 32];
        let mut out = [0f32; 32];
        for (t, chunk) in pcm.as_chunks::<32>().0.iter().enumerate().take(18) {
            slot.copy_from_slice(chunk);
            self.poly.filter(&slot, &mut out);
            for (sb, v) in out.iter().enumerate() {
                // frequency inversion: odd subbands, odd time slots (§2.4.3.4.10.5 mirrored)
                cur[sb][t] = if sb % 2 == 1 && t % 2 == 1 { -v } else { *v };
            }
        }
        let kernel = mdct_kernel();
        for sb in 0..32 {
            let mut z = [0f32; 36];
            z[..18].copy_from_slice(&self.prev[sb]);
            z[18..].copy_from_slice(&cur[sb]);
            for (k, row) in kernel.iter().enumerate() {
                xr[18 * sb + k] = row.iter().zip(&z).map(|(a, b)| a * b).sum();
            }
        }
        self.prev = cur;
        // alias reduction: the inverse of the decoder's butterflies between adjacent subbands
        for sb in 1..32 {
            for (i, (cs, ca)) in alias_coefficients().iter().enumerate() {
                let up = 18 * sb - 1 - i;
                let dn = 18 * sb + i;
                let (a, b) = (xr[up], xr[dn]);
                xr[up] = a * cs + b * ca;
                xr[dn] = b * cs - a * ca;
            }
        }
    }
}

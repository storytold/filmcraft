//! Derived tables: scan orders (6.5.3 - 6.5.5), chroma QP mapping and small helper tables.

pub use crate::spec_tables::*;

/// Up-right diagonal scan (6.5.3) of a `N x N` block as (x, y) pairs.
const fn diag<const L: usize>(n: usize) -> [(u8, u8); L] {
    let mut out = [(0u8, 0u8); L];
    let mut i = 0;
    let mut x: i32 = 0;
    let mut y: i32 = 0;
    while i < L {
        while y >= 0 {
            if (x as usize) < n && (y as usize) < n {
                out[i] = (x as u8, y as u8);
                i += 1;
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
    }
    out
}

/// Horizontal (6.5.4, `vertical == false`) or vertical (6.5.5) scan.
const fn hv<const L: usize>(n: usize, vertical: bool) -> [(u8, u8); L] {
    let mut out = [(0u8, 0u8); L];
    let mut i = 0;
    while i < L {
        let (a, b) = ((i % n) as u8, (i / n) as u8);
        out[i] = if vertical { (b, a) } else { (a, b) };
        i += 1;
    }
    out
}

pub static SCAN_1: [[(u8, u8); 1]; 3] = [[(0, 0)], [(0, 0)], [(0, 0)]];
pub static SCAN_2: [[(u8, u8); 4]; 3] = [diag::<4>(2), hv::<4>(2, false), hv::<4>(2, true)];
pub static SCAN_4: [[(u8, u8); 16]; 3] = [diag::<16>(4), hv::<16>(4, false), hv::<16>(4, true)];
pub static SCAN_8: [[(u8, u8); 64]; 3] = [diag::<64>(8), hv::<64>(8, false), hv::<64>(8, true)];

/// ScanOrder[log2BlockSize][scanIdx] (log2BlockSize 0..=3).
#[inline]
pub fn scan_order(log2: u32, scan_idx: usize) -> &'static [(u8, u8)] {
    match log2 {
        0 => &SCAN_1[scan_idx],
        1 => &SCAN_2[scan_idx],
        2 => &SCAN_4[scan_idx],
        _ => &SCAN_8[scan_idx],
    }
}

/// Inverse of a scan of an `N x N` block: scan position at `[y * N + x]`.
const fn inverse<const L: usize>(scan: [(u8, u8); L], n: usize) -> [u8; L] {
    let mut out = [0u8; L];
    let mut i = 0;
    while i < L {
        out[scan[i].1 as usize * n + scan[i].0 as usize] = i as u8;
        i += 1;
    }
    out
}

static INV_SCAN_1: [[u8; 1]; 3] = [[0], [0], [0]];
static INV_SCAN_2: [[u8; 4]; 3] = [inverse(diag::<4>(2), 2), inverse(hv::<4>(2, false), 2), inverse(hv::<4>(2, true), 2)];
static INV_SCAN_4: [[u8; 16]; 3] = [inverse(diag::<16>(4), 4), inverse(hv::<16>(4, false), 4), inverse(hv::<16>(4, true), 4)];
static INV_SCAN_8: [[u8; 64]; 3] = [inverse(diag::<64>(8), 8), inverse(hv::<64>(8, false), 8), inverse(hv::<64>(8, true), 8)];

/// Position of (x, y) in ScanOrder[log2BlockSize][scanIdx] (log2BlockSize 0..=3).
#[inline]
pub fn scan_pos(log2: u32, scan_idx: usize, x: u32, y: u32) -> usize {
    let i = ((y << log2) + x) as usize;
    (match log2 {
        0 => INV_SCAN_1[scan_idx][i],
        1 => INV_SCAN_2[scan_idx][i],
        2 => INV_SCAN_4[scan_idx][i],
        _ => INV_SCAN_8[scan_idx][i],
    }) as usize
}

/// sig_coeff_flag ctxInc (9.3.4.2.5) by scan position n inside a 4x4 sub-block, `[scanIdx][k][n]`:
/// k = prevCsbf (0..=3) gives sigCtx of blocks larger than 4x4 before the position and size
/// offsets; k = 4 gives ctxIdxMap of 4x4 blocks.
pub static SIG_CTX: [[[u8; 16]; 5]; 3] = [sig_ctx(diag::<16>(4)), sig_ctx(hv::<16>(4, false)), sig_ctx(hv::<16>(4, true))];

const fn sig_ctx(scan: [(u8, u8); 16]) -> [[u8; 16]; 5] {
    let mut out = [[0u8; 16]; 5];
    let mut n = 0;
    while n < 16 {
        let (x, y) = (scan[n].0, scan[n].1);
        out[0][n] = if x + y == 0 {
            2
        } else if x + y < 3 {
            1
        } else {
            0
        };
        out[1][n] = if y == 0 {
            2
        } else if y == 1 {
            1
        } else {
            0
        };
        out[2][n] = if x == 0 {
            2
        } else if x == 1 {
            1
        } else {
            0
        };
        out[3][n] = 2;
        out[4][n] = CTX_IDX_MAP[(y as usize) * 4 + x as usize];
        n += 1;
    }
    out
}

/// Up-right diagonal scan for log2 size 2 or 3.
pub fn scan_diag(log2: u32) -> &'static [(u8, u8)] {
    scan_order(log2, 0)
}

/// Table 8-10: QpC as a function of qPi (ChromaArrayType 1).
#[inline]
pub fn qpc_420(qpi: i32) -> i32 {
    if qpi < 30 {
        qpi
    } else if qpi > 43 {
        qpi - 6
    } else {
        QPC_30_43[(qpi - 30) as usize] as i32
    }
}

/// Table 9-50 ctxIdxMap for 4x4 sig_coeff_flag contexts.
pub const CTX_IDX_MAP: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

/// levelScale (8-309).
pub static LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// DST-VII 4x4 matrix (8.6.4.2, trType 1).
pub static DST_MATRIX: [[i8; 4]; 4] = [[29, 55, 74, 84], [74, 74, 0, -74], [84, -29, -74, 55], [55, -84, 74, -29]];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonal_scan_4x4() {
        let s = &SCAN_4[0];
        assert_eq!(&s[..6], &[(0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(s[15], (3, 3));
        assert_eq!(SCAN_2[0], [(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert_eq!(SCAN_4[2][1], (0, 1));
        assert_eq!(SCAN_4[1][1], (1, 0));
    }

    #[test]
    fn scan_pos_inverts_scan_order() {
        for log2 in 0..4 {
            for scan_idx in 0..3 {
                for (i, &(x, y)) in scan_order(log2, scan_idx).iter().enumerate() {
                    assert_eq!(scan_pos(log2, scan_idx, x as u32, y as u32), i);
                }
            }
        }
    }

    #[test]
    fn dct_matrix_properties() {
        // rows are nearly orthogonal with norm ~ 64^2 * 32
        for a in 0..32 {
            for b in 0..32 {
                let d: i64 = (0..32).map(|k| TRANS_MATRIX[a][k] as i64 * TRANS_MATRIX[b][k] as i64).sum();
                if a == b {
                    assert!((d - 131072).abs() < 1000, "row {a} norm {d}");
                } else {
                    assert!(d.abs() < 1000, "rows {a},{b}: {d}");
                }
            }
        }
        // even basis functions are symmetric, odd ones antisymmetric
        for n in 0..32 {
            for m in 0..16 {
                let (a, b) = (TRANS_MATRIX[n][m] as i32, TRANS_MATRIX[n][31 - m] as i32);
                assert_eq!(a, if n % 2 == 0 { b } else { -b }, "{n} {m}");
            }
        }
    }

    #[test]
    fn qpc_table() {
        assert_eq!(qpc_420(29), 29);
        assert_eq!(qpc_420(30), 29);
        assert_eq!(qpc_420(43), 37);
        assert_eq!(qpc_420(51), 45);
    }
}

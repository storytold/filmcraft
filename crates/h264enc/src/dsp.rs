//! Distortion metrics: SAD, SATD (4x4 Hadamard), SSD.

use crate::pixel::Pixel;

#[inline]
pub fn sad<P: Pixel>(a: &[P], sa: usize, b: &[P], sb: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..h {
        let ra = &a[r * sa..r * sa + w];
        let rb = &b[r * sb..r * sb + w];
        s += ra.iter().zip(rb).map(|(&x, &y)| x.ad(y) as u32).sum::<u32>();
    }
    s
}

/// SAD of a 16-wide block (fast path).
#[inline]
pub fn sad16<P: Pixel>(a: &[P], sa: usize, b: &[P], sb: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..h {
        let (Some(ra), Some(rb)) = (a.get(r * sa..).and_then(|x| x.first_chunk::<16>()), b.get(r * sb..).and_then(|x| x.first_chunk::<16>())) else {
            break;
        };
        let mut acc = 0u16;
        for i in 0..16 {
            acc += ra[i].ad(rb[i]);
        }
        s += acc as u32;
    }
    s
}

#[cfg(test)]
fn hadamard4x4_sum(d: &[i32; 16]) -> u32 {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a0 = d[i * 4] + d[i * 4 + 1];
        let a1 = d[i * 4] - d[i * 4 + 1];
        let a2 = d[i * 4 + 2] + d[i * 4 + 3];
        let a3 = d[i * 4 + 2] - d[i * 4 + 3];
        t[i * 4] = a0 + a2;
        t[i * 4 + 1] = a1 + a3;
        t[i * 4 + 2] = a0 - a2;
        t[i * 4 + 3] = a1 - a3;
    }
    let mut s = 0u32;
    for i in 0..4 {
        let a0 = t[i] + t[4 + i];
        let a1 = t[i] - t[4 + i];
        let a2 = t[8 + i] + t[12 + i];
        let a3 = t[8 + i] - t[12 + i];
        s += (a0 + a2).unsigned_abs() + (a1 + a3).unsigned_abs() + (a0 - a2).unsigned_abs() + (a1 - a3).unsigned_abs();
    }
    s
}

/// SATD of a strip of `N` columns (N/4 side-by-side 4x4 blocks) and 4 rows, computed lane-wise so the compiler
/// can vectorise it. Uses |p+q| + |p-q| = 2 max(|p|, |q|) for the last butterfly, which also absorbs the /2.
#[inline(always)]
fn satd_strip<P: Pixel, const N: usize, const H: usize>(a: &[P], sa: usize, b: &[P], sb: usize) -> u32 {
    let mut d = [[0i16; N]; 4];
    for r in 0..4 {
        let ra = &a[r * sa..r * sa + N];
        let rb = &b[r * sb..r * sb + N];
        for i in 0..N {
            d[r][i] = ra[i].diff(rb[i]);
        }
    }
    let mut v = [[0i16; N]; 4];
    for i in 0..N {
        let s01 = d[0][i] + d[1][i];
        let d01 = d[0][i] - d[1][i];
        let s23 = d[2][i] + d[3][i];
        let d23 = d[2][i] - d[3][i];
        v[0][i] = s01 + s23;
        v[1][i] = s01 - s23;
        v[2][i] = d01 + d23;
        v[3][i] = d01 - d23;
    }
    let mut total = 0u32;
    for row in &v {
        let mut hs = [0i16; H];
        let mut hd = [0i16; H];
        for k in 0..H {
            hs[k] = row[2 * k] + row[2 * k + 1];
            hd[k] = row[2 * k] - row[2 * k + 1];
        }
        for k in 0..H / 2 {
            total += hs[2 * k].unsigned_abs().max(hs[2 * k + 1].unsigned_abs()) as u32;
            total += hd[2 * k].unsigned_abs().max(hd[2 * k + 1].unsigned_abs()) as u32;
        }
    }
    total
}

/// Sum of absolute 4x4 Hadamard-transformed differences (halved), for w, h multiples of 4.
pub fn satd<P: Pixel>(a: &[P], sa: usize, b: &[P], sb: usize, w: usize, h: usize) -> u32 {
    let mut total = 0;
    for by in (0..h).step_by(4) {
        let (ao, bo) = (by * sa, by * sb);
        match w {
            16 => total += satd_strip::<P, 16, 8>(&a[ao..], sa, &b[bo..], sb),
            8 => total += satd_strip::<P, 8, 4>(&a[ao..], sa, &b[bo..], sb),
            4 => total += satd_strip::<P, 4, 2>(&a[ao..], sa, &b[bo..], sb),
            _ => {
                for bx in (0..w).step_by(4) {
                    total += satd_strip::<P, 4, 2>(&a[ao + bx..], sa, &b[bo + bx..], sb);
                }
            }
        }
    }
    total
}

pub fn ssd<P: Pixel>(a: &[P], sa: usize, b: &[P], sb: usize, w: usize, h: usize) -> u64 {
    let mut s = 0u64;
    for r in 0..h {
        let ra = &a[r * sa..r * sa + w];
        let rb = &b[r * sb..r * sb + w];
        s += ra.iter().zip(rb).map(|(&x, &y)| (x.get() - y.get()).pow(2) as u32).sum::<u32>() as u64;
    }
    s
}

/// Variance*N (sum of squares minus square of sum / N) of a block.
pub fn block_var<P: Pixel>(a: &[P], sa: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    let mut ss = 0u32;
    for r in 0..h {
        for &v in &a[r * sa..r * sa + w] {
            let v = v.get() as u32;
            s += v;
            ss += v * v;
        }
    }
    ss - ((s as u64 * s as u64) / (w * h) as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn satd_of_dc_difference() {
        let a = [10u8; 16];
        let b = [0u8; 16];
        // DC-only difference of 10: hadamard DC = 160, halved = 80.
        assert_eq!(satd(&a, 4, &b, 4, 4, 4), 80);
        assert_eq!(sad(&a, 4, &b, 4, 4, 4), 160);
    }

    #[test]
    fn satd_matches_reference() {
        let mut seed = 7u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        let a: Vec<u8> = (0..16 * 16).map(|_| rnd()).collect();
        let b: Vec<u8> = (0..16 * 16).map(|_| rnd()).collect();
        for (w, h) in [(16, 16), (8, 8), (4, 4), (16, 8), (8, 16), (12, 4)] {
            let mut want = 0;
            for by in (0..h).step_by(4) {
                for bx in (0..w).step_by(4) {
                    let mut d = [0i32; 16];
                    for r in 0..4 {
                        for c in 0..4 {
                            d[r * 4 + c] = a[(by + r) * 16 + bx + c] as i32 - b[(by + r) * 16 + bx + c] as i32;
                        }
                    }
                    want += hadamard4x4_sum(&d);
                }
            }
            assert_eq!(satd(&a, 16, &b, 16, w, h), want / 2, "{w}x{h}");
        }
    }
}

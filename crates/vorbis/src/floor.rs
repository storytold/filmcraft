//! Floor type 1 (Vorbis I §7): a piecewise-linear spectral envelope in the decoder's dB scale.
//!
//! The encoder picks a target value at each of the floor's X positions, codes it against the
//! decoder's prediction from its neighbours (step 2 of §7.2.4, inverted), and renders the curve
//! exactly as the decoder will (`render_line`, integer arithmetic), so the residue is computed
//! against the very floor the decoder multiplies it by.

use crate::book::{BitWriter, Codebook, ilog};

/// `floor1_multiplier`: 2, so values run 0..128 and step two entries of the dB table.
pub const MULTIPLIER: u32 = 2;
/// The value range for [`MULTIPLIER`] 2.
pub const RANGE: i32 = 128;
const RANGE_BITS: u32 = 10;
/// Points per partition, and partitions.
pub const CLASS_DIM: usize = 4;
pub const PARTITIONS: usize = 8;

/// Step (in dB) between entries of `floor1_inverse_dB_table`: it runs from 1.0649863e-07 to 1
/// over 256 entries.
fn db_step() -> f64 {
    (1.0f64 / 1.064_986_3e-7).ln() / 255.0
}

/// `floor1_inverse_dB_table[i]`.
pub fn inverse_db(i: i32) -> f32 {
    (f64::from(i.clamp(0, 255) - 255) * db_step()).exp() as f32
}

/// The table index whose amplitude is nearest (in dB) to `a`.
pub fn db_index(a: f32) -> i32 {
    if !a.is_finite() || a <= 0.0 {
        return 0;
    }
    ((f64::from(a)).ln() / db_step() + 255.0).round().clamp(0.0, 255.0) as i32
}

/// A floor 1 configuration for blocks of `n/2 = 2^RANGE_BITS` bins.
#[derive(Clone, Debug)]
pub struct Floor1 {
    /// X positions: 0, 1024, then the partitions' points.
    pub xs: Vec<i32>,
    /// Index of the book coding the point values.
    pub book: usize,
    /// For each point (in coding order) its low and high neighbours.
    neighbours: Vec<(usize, usize)>,
    /// Point indices sorted by X.
    sorted: Vec<usize>,
}

impl Floor1 {
    /// Points spaced about evenly in log frequency over 1..1024.
    pub fn new(book: usize) -> Floor1 {
        let count = CLASS_DIM * PARTITIONS;
        let mut xs = vec![0, 1 << RANGE_BITS];
        let mut last = 0;
        for i in 1..=count {
            // geometric from 2 to ~1000 bins, at least one bin apart
            let x = (2.0f64 * (500.0f64).powf((i - 1) as f64 / (count - 1) as f64)).round() as i32;
            let x = x.max(last + 1).min((1 << RANGE_BITS) - 1);
            xs.push(x);
            last = x;
        }
        let neighbours = (0..xs.len())
            .map(|i| {
                if i < 2 {
                    return (0, 0);
                }
                let (mut lo, mut hi) = (0usize, 1usize);
                for j in 0..i {
                    if xs[j] < xs[i] && xs[j] > xs[lo] {
                        lo = j;
                    }
                    if xs[j] > xs[i] && xs[j] < xs[hi] {
                        hi = j;
                    }
                }
                (lo, hi)
            })
            .collect();
        let mut sorted: Vec<usize> = (0..xs.len()).collect();
        sorted.sort_by_key(|&i| xs[i]);
        Floor1 { xs, book, neighbours, sorted }
    }

    /// The setup-header encoding (§7.2.2): one class of `CLASS_DIM` points, no subclasses.
    pub fn write_header(&self, w: &mut BitWriter) {
        w.write(PARTITIONS as u32, 5);
        for _ in 0..PARTITIONS {
            w.write(0, 4);
        }
        w.write(CLASS_DIM as u32 - 1, 3);
        w.write(0, 2); // no subclasses
        w.write(self.book as u32 + 1, 8);
        w.write(MULTIPLIER - 1, 2);
        w.write(RANGE_BITS, 4);
        for &x in &self.xs[2..] {
            w.write(x as u32, RANGE_BITS);
        }
    }

    /// Codes the target values `want` (one per point, 0..RANGE) into `w`; returns the floor curve
    /// the decoder renders (`n2` amplitudes).
    pub fn encode(&self, w: &mut BitWriter, book: &Codebook, want: &[i32], n2: usize) -> Vec<f32> {
        let n = self.xs.len();
        let want = |i: usize| want.get(i).copied().unwrap_or(0).clamp(0, RANGE - 1);
        let mut fin = vec![0i32; n];
        let mut used = vec![false; n];
        let mut coded = vec![0u32; n];
        fin[0] = want(0);
        fin[1] = want(1);
        used[0] = true;
        used[1] = true;
        for i in 2..n {
            let (lo, hi) = self.neighbours[i];
            let pred = render_point(self.xs[lo], fin[lo], self.xs[hi], fin[hi], self.xs[i]);
            let f = want(i);
            let (highroom, lowroom) = (RANGE - pred, pred);
            let room = 2 * highroom.min(lowroom);
            let val = if f == pred {
                0
            } else {
                let d = f - pred;
                let inside = if d > 0 { 2 * d } else { -2 * d - 1 };
                if inside < room {
                    inside
                } else if highroom > lowroom {
                    f - pred + lowroom
                } else {
                    pred - f + highroom - 1
                }
            };
            coded[i] = val.clamp(0, RANGE - 1) as u32;
            if val != 0 {
                used[lo] = true;
                used[hi] = true;
                used[i] = true;
                fin[i] = f;
            } else {
                fin[i] = pred;
            }
        }
        let bits = ilog(RANGE as u32 - 1);
        w.write(fin[0] as u32, bits);
        w.write(fin[1] as u32, bits);
        for &c in &coded[2..] {
            book.put(w, c as usize);
        }
        self.render(&fin, &used, n2)
    }

    /// The decoder's curve (§7.2.4 step 2, `render_line`) as amplitudes.
    fn render(&self, fin: &[i32], used: &[bool], n2: usize) -> Vec<f32> {
        let mut v = vec![0i32; n2];
        let m = MULTIPLIER as i32;
        let (mut lx, mut ly) = (0i32, fin[self.sorted[0]] * m);
        let (mut hx, mut hy) = (0i32, ly);
        for &i in &self.sorted[1..] {
            if used[i] {
                hy = fin[i] * m;
                hx = self.xs[i];
                render_line(lx, ly, hx, hy, &mut v);
                lx = hx;
                ly = hy;
            }
        }
        if (hx as usize) < n2 {
            render_line(hx, hy, n2 as i32, hy, &mut v);
        }
        v.iter().map(|&i| inverse_db(i)).collect()
    }
}

fn render_point(x0: i32, y0: i32, x1: i32, y1: i32, x: i32) -> i32 {
    let dy = y1 - y0;
    let adx = (x1 - x0).max(1);
    let err = dy.abs() * (x - x0);
    let off = err / adx;
    if dy < 0 { y0 - off } else { y0 + off }
}

fn render_line(x0: i32, y0: i32, x1: i32, y1: i32, v: &mut [i32]) {
    let dy = y1 - y0;
    let adx = x1 - x0;
    if adx <= 0 {
        return;
    }
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = dy.abs() - base.abs() * adx;
    let (mut y, mut err) = (y0, 0);
    if let Some(s) = v.get_mut(x0 as usize) {
        *s = y;
    }
    for x in x0 + 1..x1 {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        if let Some(s) = v.get_mut(x as usize) {
            *s = y;
        }
    }
}

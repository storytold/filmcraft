//! Intra prediction (spec 7.11.2), palette prediction (7.11.4) and chroma from luma (7.11.5).

use crate::frame::Plane;
use crate::spec_tables::{
    ANGLE_STEP, D67_PRED, DC_PRED, DR_INTRA_DERIVATIVE, INTRA_EDGE_KERNEL, INTRA_EDGE_TAPS, INTRA_FILTER_SCALE_BITS, INTRA_FILTER_TAPS, MODE_TO_ANGLE,
    SM_WEIGHTS_TX_4X4, SM_WEIGHTS_TX_8X8, SM_WEIGHTS_TX_16X16, SM_WEIGHTS_TX_32X32, SM_WEIGHTS_TX_64X64, SMOOTH_H_PRED, SMOOTH_PRED, SMOOTH_V_PRED, V_PRED,
};
use crate::transform::round2;

/// Inputs of the intra prediction process that come from the block.
pub(crate) struct IntraParams {
    pub plane: usize,
    pub x: usize,
    pub y: usize,
    pub have_left: bool,
    pub have_above: bool,
    pub have_above_right: bool,
    pub have_below_left: bool,
    pub mode: usize,
    pub log2w: u32,
    pub log2h: u32,
    /// Largest valid x / y in the plane: ((MiCols * MI_SIZE) >> subX) - 1.
    pub max_x: i32,
    pub max_y: i32,
    pub bit_depth: u32,
    pub angle_delta: i32,
    pub use_filter_intra: bool,
    pub filter_intra_mode: usize,
    pub enable_intra_edge_filter: bool,
    /// Result of the intra filter type process (7.11.2.8).
    pub filter_type: bool,
}

const EDGE: usize = 32; // offset of index 0 in the edge buffers (they start at index -2 .. -1)

#[inline(always)]
fn clip1(v: i32, bd: u32) -> u16 {
    v.clamp(0, (1 << bd) - 1) as u16
}

pub(crate) fn is_directional_mode(mode: usize) -> bool {
    (V_PRED..=D67_PRED).contains(&mode)
}

/// Predict the w x h block at (x, y) of `pl` (writes into the plane).
pub(crate) fn predict_intra(pl: &mut Plane, p: &IntraParams) {
    let w = 1usize << p.log2w;
    let h = 1usize << p.log2h;
    let bd = p.bit_depth;
    let (x, y) = (p.x, p.y);
    // edge arrays with room for index -2 .. 2*(w+h)
    let mut above = [0i32; 2 * 128 + 2 * EDGE];
    let mut left = [0i32; 2 * 128 + 2 * EDGE];
    let n = w + h;
    if !p.have_above && p.have_left {
        let v = pl.at(x - 1, y) as i32;
        above[EDGE..EDGE + n].iter_mut().for_each(|a| *a = v);
    } else if !p.have_above && !p.have_left {
        let v = (1 << (bd - 1)) - 1;
        above[EDGE..EDGE + n].iter_mut().for_each(|a| *a = v);
    } else {
        let above_limit = (p.max_x).min(x as i32 + if p.have_above_right { 2 * w as i32 } else { w as i32 } - 1);
        let row = pl.row_from(y - 1, x);
        for i in 0..n {
            above[EDGE + i] = row[(above_limit.min(x as i32 + i as i32)) as usize - x] as i32;
        }
    }
    if !p.have_left && p.have_above {
        let v = pl.at(x, y - 1) as i32;
        left[EDGE..EDGE + n].iter_mut().for_each(|a| *a = v);
    } else if !p.have_left && !p.have_above {
        let v = (1 << (bd - 1)) + 1;
        left[EDGE..EDGE + n].iter_mut().for_each(|a| *a = v);
    } else {
        let left_limit = (p.max_y).min(y as i32 + if p.have_below_left { 2 * h as i32 } else { h as i32 } - 1);
        for i in 0..n {
            left[EDGE + i] = pl.at(x - 1, left_limit.min(y as i32 + i as i32) as usize) as i32;
        }
    }
    let corner = if p.have_above && p.have_left {
        pl.at(x - 1, y - 1) as i32
    } else if p.have_above {
        pl.at(x, y - 1) as i32
    } else if p.have_left {
        pl.at(x - 1, y) as i32
    } else {
        1 << (bd - 1)
    };
    above[EDGE - 1] = corner;
    left[EDGE - 1] = corner;

    let mut pred = [0u16; 64 * 64];
    if p.plane == 0 && p.use_filter_intra {
        recursive_intra(&above, &left, w, h, p.filter_intra_mode, bd, &mut pred);
    } else if is_directional_mode(p.mode) {
        directional(&mut above, &mut left, p, w, h, &mut pred);
    } else if p.mode == SMOOTH_PRED || p.mode == SMOOTH_V_PRED || p.mode == SMOOTH_H_PRED {
        smooth(&above, &left, p, &mut pred);
    } else if p.mode == DC_PRED {
        dc(&above, &left, p, &mut pred);
    } else {
        // PAETH_PRED
        let tl = above[EDGE - 1];
        for i in 0..h {
            for j in 0..w {
                let a = above[EDGE + j];
                let l = left[EDGE + i];
                let base = a + l - tl;
                let p_left = (base - l).abs();
                let p_top = (base - a).abs();
                let p_tl = (base - tl).abs();
                pred[i * w + j] = if p_left <= p_top && p_left <= p_tl {
                    l as u16
                } else if p_top <= p_tl {
                    a as u16
                } else {
                    tl as u16
                };
            }
        }
    }
    for i in 0..h {
        pl.row_from_mut(y + i, x)[..w].copy_from_slice(&pred[i * w..i * w + w]);
    }
}

fn recursive_intra(above: &[i32], left: &[i32], w: usize, h: usize, mode: usize, bd: u32, pred: &mut [u16]) {
    let w4 = w >> 2;
    let h2 = h >> 1;
    for i2 in 0..h2 {
        for j4 in 0..w4 {
            let mut pv = [0i32; 7];
            for (i, v) in pv.iter_mut().enumerate() {
                *v = if i < 5 {
                    if i2 == 0 {
                        above[EDGE + (j4 << 2) + i - 1]
                    } else if j4 == 0 && i == 0 {
                        left[EDGE + (i2 << 1) - 1]
                    } else {
                        pred[((i2 << 1) - 1) * w + (j4 << 2) + i - 1] as i32
                    }
                } else if j4 == 0 {
                    left[EDGE + (i2 << 1) + i - 5]
                } else {
                    pred[((i2 << 1) + i - 5) * w + (j4 << 2) - 1] as i32
                };
            }
            for i1 in 0..2 {
                for j1 in 0..4 {
                    let mut pr = 0i32;
                    for i in 0..7 {
                        pr += INTRA_FILTER_TAPS[mode][(i1 << 2) + j1][i] as i32 * pv[i];
                    }
                    pred[((i2 << 1) + i1) * w + (j4 << 2) + j1] = clip1(round2_signed(pr, INTRA_FILTER_SCALE_BITS as u32), bd);
                }
            }
        }
    }
}

#[inline(always)]
pub(crate) fn round2_signed(x: i32, n: u32) -> i32 {
    if x >= 0 { round2(x, n) } else { -round2(-x, n) }
}

fn sm_weights(log2: u32) -> &'static [u8] {
    match log2 {
        2 => &SM_WEIGHTS_TX_4X4,
        3 => &SM_WEIGHTS_TX_8X8,
        4 => &SM_WEIGHTS_TX_16X16,
        5 => &SM_WEIGHTS_TX_32X32,
        _ => &SM_WEIGHTS_TX_64X64,
    }
}

fn smooth(above: &[i32], left: &[i32], p: &IntraParams, pred: &mut [u16]) {
    let (mode, log2w, log2h) = (p.mode, p.log2w, p.log2h);
    let (w, h) = (1usize << log2w, 1usize << log2h);
    let wx = sm_weights(log2w);
    let wy = sm_weights(log2h);
    for i in 0..h {
        for j in 0..w {
            let v = if mode == SMOOTH_PRED {
                let s = wy[i] as i32 * above[EDGE + j]
                    + (256 - wy[i] as i32) * left[EDGE + h - 1]
                    + wx[j] as i32 * left[EDGE + i]
                    + (256 - wx[j] as i32) * above[EDGE + w - 1];
                round2(s, 9)
            } else if mode == SMOOTH_V_PRED {
                round2(wy[i] as i32 * above[EDGE + j] + (256 - wy[i] as i32) * left[EDGE + h - 1], 8)
            } else {
                round2(wx[j] as i32 * left[EDGE + i] + (256 - wx[j] as i32) * above[EDGE + w - 1], 8)
            };
            pred[i * w + j] = v as u16;
        }
    }
}

fn dc(above: &[i32], left: &[i32], p: &IntraParams, pred: &mut [u16]) {
    let (have_left, have_above, log2w, log2h, bd) = (p.have_left, p.have_above, p.log2w, p.log2h, p.bit_depth);
    let (w, h) = (1usize << log2w, 1usize << log2h);
    let v = if have_left && have_above {
        let mut sum: i32 = left[EDGE..EDGE + h].iter().sum::<i32>() + above[EDGE..EDGE + w].iter().sum::<i32>();
        sum += ((w + h) >> 1) as i32;
        sum / (w + h) as i32
    } else if have_left {
        let sum: i32 = left[EDGE..EDGE + h].iter().sum();
        (sum + (h >> 1) as i32) >> log2h
    } else if have_above {
        let sum: i32 = above[EDGE..EDGE + w].iter().sum();
        (sum + (w >> 1) as i32) >> log2w
    } else {
        1 << (bd - 1)
    };
    let v = clip1(v, bd);
    pred[..w * h].iter_mut().for_each(|p| *p = v);
}

fn edge_filter_strength(w: usize, h: usize, filter_type: bool, delta: i32) -> u32 {
    let d = delta.abs();
    let blk_wh = w + h;
    let mut strength = 0;
    if !filter_type {
        if blk_wh <= 8 {
            if d >= 56 {
                strength = 1;
            }
        } else if blk_wh <= 16 {
            if d >= 40 {
                strength = 1;
            }
        } else if blk_wh <= 24 {
            if d >= 8 {
                strength = 1;
            }
            if d >= 16 {
                strength = 2;
            }
            if d >= 32 {
                strength = 3;
            }
        } else if blk_wh <= 32 {
            strength = 1;
            if d >= 4 {
                strength = 2;
            }
            if d >= 32 {
                strength = 3;
            }
        } else {
            strength = 3;
        }
    } else if blk_wh <= 8 {
        if d >= 40 {
            strength = 1;
        }
        if d >= 64 {
            strength = 2;
        }
    } else if blk_wh <= 16 {
        if d >= 20 {
            strength = 1;
        }
        if d >= 48 {
            strength = 2;
        }
    } else if blk_wh <= 24 {
        if d >= 4 {
            strength = 3;
        }
    } else {
        strength = 3;
    }
    strength
}

fn use_upsample(w: usize, h: usize, filter_type: bool, delta: i32) -> bool {
    let d = delta.abs();
    let blk_wh = w + h;
    if d <= 0 || d >= 40 {
        false
    } else if !filter_type {
        blk_wh <= 16
    } else {
        blk_wh <= 8
    }
}

/// Intra edge filter (7.11.2.12) on buf[-1 .. sz-2] (edge[i] = buf[i - 1]).
fn edge_filter(buf: &mut [i32], sz: usize, strength: u32) {
    if strength == 0 {
        return;
    }
    let mut edge = [0i32; 2 * 128 + 2 * EDGE];
    for i in 0..sz {
        edge[i] = buf[EDGE + i - 1];
    }
    for i in 1..sz {
        let mut s = 0;
        for j in 0..INTRA_EDGE_TAPS {
            let k = (i as i32 - 2 + j as i32).clamp(0, sz as i32 - 1) as usize;
            s += INTRA_EDGE_KERNEL[strength as usize - 1][j] as i32 * edge[k];
        }
        buf[EDGE + i - 1] = (s + 8) >> 4;
    }
}

/// Intra edge upsample (7.11.2.11).
fn edge_upsample(buf: &mut [i32], num_px: usize, bd: u32) {
    let mut dup = [0i32; 2 * 128 + 8];
    dup[0] = buf[EDGE - 1];
    for i in -1..num_px as i32 {
        dup[(i + 2) as usize] = buf[(EDGE as i32 + i) as usize];
    }
    dup[num_px + 2] = buf[EDGE + num_px - 1];
    buf[EDGE - 2] = dup[0];
    for i in 0..num_px {
        let s = -dup[i] + 9 * dup[i + 1] + 9 * dup[i + 2] - dup[i + 3];
        let s = clip1(round2(s, 4), bd) as i32;
        buf[EDGE + 2 * i - 1] = s;
        buf[EDGE + 2 * i] = dup[i + 2];
    }
}

fn directional(above: &mut [i32], left: &mut [i32], p: &IntraParams, w: usize, h: usize, pred: &mut [u16]) {
    let p_angle = MODE_TO_ANGLE[p.mode] as i32 + p.angle_delta * ANGLE_STEP as i32;
    let mut upsample_above = 0u32;
    let mut upsample_left = 0u32;
    if p.enable_intra_edge_filter {
        if p_angle != 90 && p_angle != 180 {
            if p_angle > 90 && p_angle < 180 && (w + h) >= 24 {
                let s = left[EDGE] * 5 + above[EDGE - 1] * 6 + above[EDGE] * 5;
                let c = round2(s, 4);
                left[EDGE - 1] = c;
                above[EDGE - 1] = c;
            }
            if p.have_above {
                let strength = edge_filter_strength(w, h, p.filter_type, p_angle - 90);
                let num_px = w.min((p.max_x - p.x as i32 + 1) as usize) + if p_angle < 90 { h } else { 0 } + 1;
                edge_filter(above, num_px, strength);
            }
            if p.have_left {
                let strength = edge_filter_strength(w, h, p.filter_type, p_angle - 180);
                let num_px = h.min((p.max_y - p.y as i32 + 1) as usize) + if p_angle > 180 { w } else { 0 } + 1;
                edge_filter(left, num_px, strength);
            }
        }
        if use_upsample(w, h, p.filter_type, p_angle - 90) {
            upsample_above = 1;
            let num_px = w + if p_angle < 90 { h } else { 0 };
            edge_upsample(above, num_px, p.bit_depth);
        }
        if use_upsample(w, h, p.filter_type, p_angle - 180) {
            upsample_left = 1;
            let num_px = h + if p_angle > 180 { w } else { 0 };
            edge_upsample(left, num_px, p.bit_depth);
        }
    }
    let dx = if p_angle < 90 {
        DR_INTRA_DERIVATIVE[p_angle as usize] as i32
    } else if p_angle > 90 && p_angle < 180 {
        DR_INTRA_DERIVATIVE[(180 - p_angle) as usize] as i32
    } else {
        0
    };
    let dy = if p_angle > 90 && p_angle < 180 {
        DR_INTRA_DERIVATIVE[(p_angle - 90) as usize] as i32
    } else if p_angle > 180 {
        DR_INTRA_DERIVATIVE[(270 - p_angle) as usize] as i32
    } else {
        0
    };
    let a = |i: i32| above[(EDGE as i32 + i) as usize];
    let l = |i: i32| left[(EDGE as i32 + i) as usize];
    for i in 0..h as i32 {
        for j in 0..w as i32 {
            let v = if p_angle < 90 {
                let idx = (i + 1) * dx;
                let base = (idx >> (6 - upsample_above)) + (j << upsample_above);
                let shift = ((idx << upsample_above) >> 1) & 0x1f;
                let max_base_x = (w as i32 + h as i32 - 1) << upsample_above;
                if base < max_base_x { round2(a(base) * (32 - shift) + a(base + 1) * shift, 5) } else { a(max_base_x) }
            } else if p_angle > 90 && p_angle < 180 {
                let idx = (j << 6) - (i + 1) * dx;
                let base = idx >> (6 - upsample_above);
                if base >= -(1 << upsample_above) {
                    let shift = ((idx << upsample_above) >> 1) & 0x1f;
                    round2(a(base) * (32 - shift) + a(base + 1) * shift, 5)
                } else {
                    let idx = (i << 6) - (j + 1) * dy;
                    let base = idx >> (6 - upsample_left);
                    let shift = ((idx << upsample_left) >> 1) & 0x1f;
                    round2(l(base) * (32 - shift) + l(base + 1) * shift, 5)
                }
            } else if p_angle > 180 {
                let idx = (j + 1) * dy;
                let base = (idx >> (6 - upsample_left)) + (i << upsample_left);
                let shift = ((idx << upsample_left) >> 1) & 0x1f;
                round2(l(base) * (32 - shift) + l(base + 1) * shift, 5)
            } else if p_angle == 90 {
                a(j)
            } else {
                l(i)
            };
            pred[(i as usize) * w + j as usize] = v as u16;
        }
    }
}

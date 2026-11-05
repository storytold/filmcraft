//! Probability tables (a "frame context"), syntax element counters and the backward adaptation
//! processes of section 8.4.

use crate::tables::{
    BINARY_TREE, COUNT_SAT, DEFAULT_COEF_PROBS, DEFAULT_COMP_MODE_PROB, DEFAULT_COMP_REF_PROB, DEFAULT_INTER_MODE_PROBS, DEFAULT_INTERP_FILTER_PROBS,
    DEFAULT_IS_INTER_PROB, DEFAULT_MV_BITS_PROB, DEFAULT_MV_CLASS_PROBS, DEFAULT_MV_CLASS0_BIT_PROB, DEFAULT_MV_CLASS0_FR_PROBS, DEFAULT_MV_CLASS0_HP_PROB,
    DEFAULT_MV_FR_PROBS, DEFAULT_MV_HP_PROB, DEFAULT_MV_JOINT_PROBS, DEFAULT_MV_SIGN_PROB, DEFAULT_PARTITION_PROBS, DEFAULT_SINGLE_REF_PROB, DEFAULT_SKIP_PROB,
    DEFAULT_TX_PROBS, DEFAULT_UV_MODE_PROBS, DEFAULT_Y_MODE_PROBS, INTER_MODE_TREE, INTERP_FILTER_TREE, INTRA_MODE_TREE, MAX_UPDATE_FACTOR, MV_CLASS_TREE,
    MV_FR_TREE, MV_JOINT_TREE, PARTITION_TREE, SMALL_TOKEN_TREE, TX_SIZE_8_TREE, TX_SIZE_16_TREE, TX_SIZE_32_TREE,
};

/// coef_probs[txSz][plane > 0][is_inter][band][ctx][node].
pub type CoefProbs = [[[[[[u8; 3]; 6]; 6]; 2]; 2]; 4];

/// All adaptive probabilities of one frame context (10.5).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameContext {
    /// tx_probs[maxTxSize][ctx][node] (index 0 unused).
    pub tx: [[[u8; 3]; 2]; 4],
    pub coef: CoefProbs,
    pub skip: [u8; 3],
    pub inter_mode: [[u8; 3]; 7],
    pub interp_filter: [[u8; 2]; 4],
    pub is_inter: [u8; 4],
    pub comp_mode: [u8; 5],
    pub single_ref: [[u8; 2]; 5],
    pub comp_ref: [u8; 5],
    pub y_mode: [[u8; 9]; 4],
    pub uv_mode: [[u8; 9]; 10],
    pub partition: [[u8; 3]; 16],
    pub mv_joint: [u8; 3],
    pub mv_sign: [u8; 2],
    pub mv_class: [[u8; 10]; 2],
    pub mv_class0_bit: [u8; 2],
    pub mv_bits: [[u8; 10]; 2],
    pub mv_class0_fr: [[[u8; 3]; 2]; 2],
    pub mv_fr: [[u8; 3]; 2],
    pub mv_class0_hp: [u8; 2],
    pub mv_hp: [u8; 2],
}

fn arr<const N: usize>(s: &[u8]) -> [u8; N] {
    s.first_chunk::<N>().copied().unwrap_or([128; N])
}

impl Default for FrameContext {
    fn default() -> Self {
        let mut coef = [[[[[[0u8; 3]; 6]; 6]; 2]; 2]; 4];
        let mut k = 0;
        for t in coef.iter_mut() {
            for i in t.iter_mut() {
                for j in i.iter_mut() {
                    for b in j.iter_mut() {
                        for c in b.iter_mut() {
                            *c = arr(&DEFAULT_COEF_PROBS[k..k + 3]);
                            k += 3;
                        }
                    }
                }
            }
        }
        let mut tx = [[[0u8; 3]; 2]; 4];
        for (m, t) in tx.iter_mut().enumerate() {
            for (c, p) in t.iter_mut().enumerate() {
                *p = arr(&DEFAULT_TX_PROBS[(m * 2 + c) * 3..(m * 2 + c) * 3 + 3]);
            }
        }
        FrameContext {
            tx,
            coef,
            skip: DEFAULT_SKIP_PROB,
            inter_mode: std::array::from_fn(|i| arr(&DEFAULT_INTER_MODE_PROBS[i * 3..i * 3 + 3])),
            interp_filter: std::array::from_fn(|i| arr(&DEFAULT_INTERP_FILTER_PROBS[i * 2..i * 2 + 2])),
            is_inter: DEFAULT_IS_INTER_PROB,
            comp_mode: DEFAULT_COMP_MODE_PROB,
            single_ref: std::array::from_fn(|i| arr(&DEFAULT_SINGLE_REF_PROB[i * 2..i * 2 + 2])),
            comp_ref: DEFAULT_COMP_REF_PROB,
            y_mode: std::array::from_fn(|i| arr(&DEFAULT_Y_MODE_PROBS[i * 9..i * 9 + 9])),
            uv_mode: std::array::from_fn(|i| arr(&DEFAULT_UV_MODE_PROBS[i * 9..i * 9 + 9])),
            partition: std::array::from_fn(|i| arr(&DEFAULT_PARTITION_PROBS[i * 3..i * 3 + 3])),
            mv_joint: DEFAULT_MV_JOINT_PROBS,
            mv_sign: DEFAULT_MV_SIGN_PROB,
            mv_class: std::array::from_fn(|i| arr(&DEFAULT_MV_CLASS_PROBS[i * 10..i * 10 + 10])),
            mv_class0_bit: DEFAULT_MV_CLASS0_BIT_PROB,
            mv_bits: std::array::from_fn(|i| arr(&DEFAULT_MV_BITS_PROB[i * 10..i * 10 + 10])),
            mv_class0_fr: std::array::from_fn(|i| std::array::from_fn(|j| arr(&DEFAULT_MV_CLASS0_FR_PROBS[(i * 2 + j) * 3..(i * 2 + j) * 3 + 3]))),
            mv_fr: std::array::from_fn(|i| arr(&DEFAULT_MV_FR_PROBS[i * 3..i * 3 + 3])),
            mv_class0_hp: DEFAULT_MV_CLASS0_HP_PROB,
            mv_hp: DEFAULT_MV_HP_PROB,
        }
    }
}

/// Syntax element counters (8.3).
#[derive(Clone, Default)]
pub struct Counts {
    pub intra_mode: [[u32; 10]; 4],
    pub uv_mode: [[u32; 10]; 10],
    pub partition: [[u32; 4]; 16],
    pub interp_filter: [[u32; 3]; 4],
    pub inter_mode: [[u32; 4]; 7],
    /// tx_size[maxTxSize][ctx][value].
    pub tx: [[[u32; 4]; 2]; 4],
    pub is_inter: [[u32; 2]; 4],
    pub comp_mode: [[u32; 2]; 5],
    pub single_ref: [[[u32; 2]; 2]; 5],
    pub comp_ref: [[u32; 2]; 5],
    pub skip: [[u32; 2]; 3],
    pub mv_joint: [u32; 4],
    pub mv_sign: [[u32; 2]; 2],
    pub mv_class: [[u32; 11]; 2],
    pub mv_class0_bit: [[u32; 2]; 2],
    pub mv_class0_fr: [[[u32; 4]; 2]; 2],
    pub mv_class0_hp: [[u32; 2]; 2],
    pub mv_bits: [[[u32; 2]; 10]; 2],
    pub mv_fr: [[u32; 4]; 2],
    pub mv_hp: [[u32; 2]; 2],
    /// token[txSz][plane>0][is_inter][band][ctx][min(2, token)].
    pub token: [[[[[[u32; 3]; 6]; 6]; 2]; 2]; 4],
    /// more_coefs[txSz][plane>0][is_inter][band][ctx][value].
    pub more_coefs: [[[[[[u32; 2]; 6]; 6]; 2]; 2]; 4],
}

fn add_slice(a: &mut [u32], b: &[u32]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x += y;
    }
}

macro_rules! add_nested {
    ($a:expr, $b:expr, 1) => {
        add_slice(&mut $a[..], &$b[..])
    };
    ($a:expr, $b:expr, 2) => {
        for (x, y) in $a.iter_mut().zip($b.iter()) {
            add_slice(&mut x[..], &y[..]);
        }
    };
    ($a:expr, $b:expr, 3) => {
        for (x, y) in $a.iter_mut().zip($b.iter()) {
            add_nested!(x, y, 2);
        }
    };
    ($a:expr, $b:expr, 6) => {
        for (x, y) in $a.iter_mut().zip($b.iter()) {
            for (x, y) in x.iter_mut().zip(y.iter()) {
                for (x, y) in x.iter_mut().zip(y.iter()) {
                    for (x, y) in x.iter_mut().zip(y.iter()) {
                        add_nested!(x, y, 2);
                    }
                }
            }
        }
    };
}

impl Counts {
    /// Accumulate the counts of another tile.
    pub fn add(&mut self, o: &Counts) {
        add_nested!(self.intra_mode, o.intra_mode, 2);
        add_nested!(self.uv_mode, o.uv_mode, 2);
        add_nested!(self.partition, o.partition, 2);
        add_nested!(self.interp_filter, o.interp_filter, 2);
        add_nested!(self.inter_mode, o.inter_mode, 2);
        add_nested!(self.tx, o.tx, 3);
        add_nested!(self.is_inter, o.is_inter, 2);
        add_nested!(self.comp_mode, o.comp_mode, 2);
        add_nested!(self.single_ref, o.single_ref, 3);
        add_nested!(self.comp_ref, o.comp_ref, 2);
        add_nested!(self.skip, o.skip, 2);
        add_nested!(self.mv_joint, o.mv_joint, 1);
        add_nested!(self.mv_sign, o.mv_sign, 2);
        add_nested!(self.mv_class, o.mv_class, 2);
        add_nested!(self.mv_class0_bit, o.mv_class0_bit, 2);
        add_nested!(self.mv_class0_fr, o.mv_class0_fr, 3);
        add_nested!(self.mv_class0_hp, o.mv_class0_hp, 2);
        add_nested!(self.mv_bits, o.mv_bits, 3);
        add_nested!(self.mv_fr, o.mv_fr, 2);
        add_nested!(self.mv_hp, o.mv_hp, 2);
        self.add_coef_counts(o);
    }

    /// Accumulate the per-coefficient token and more_coefs counts of another tile.
    fn add_coef_counts(&mut self, o: &Counts) {
        add_nested!(self.token, o.token, 6);
        add_nested!(self.more_coefs, o.more_coefs, 6);
    }
}

/// merge_prob (8.4.1).
fn merge_prob(pre: u8, ct0: u32, ct1: u32, count_sat: u32, max_update_factor: u32) -> u8 {
    let den = ct0 + ct1;
    let prob = if den == 0 { 128 } else { ((ct0 as u64 * 256 + (den as u64 >> 1)) / den as u64).clamp(1, 255) as u32 };
    let count = den.min(count_sat);
    let factor = max_update_factor * count / count_sat;
    ((pre as u32 * (256 - factor) + prob * factor + 128) >> 8) as u8
}

/// merge_probs (8.4.2).
fn merge_probs(tree: &[i8], i: usize, probs: &mut [u8], counts: &[u32], count_sat: u32, max_update_factor: u32) -> u32 {
    let s = tree[i];
    let left = if s <= 0 { counts[(-s) as usize] } else { merge_probs(tree, s as usize, probs, counts, count_sat, max_update_factor) };
    let r = tree[i + 1];
    let right = if r <= 0 { counts[(-r) as usize] } else { merge_probs(tree, r as usize, probs, counts, count_sat, max_update_factor) };
    probs[i >> 1] = merge_prob(probs[i >> 1], left, right, count_sat, max_update_factor);
    left + right
}

fn adapt_probs(tree: &[i8], probs: &mut [u8], counts: &[u32]) {
    merge_probs(tree, 0, probs, counts, COUNT_SAT, MAX_UPDATE_FACTOR);
}

fn adapt_prob(prob: &mut u8, counts: &[u32; 2]) {
    *prob = merge_prob(*prob, counts[0], counts[1], COUNT_SAT, MAX_UPDATE_FACTOR);
}

/// Coefficient probability adaptation (8.4.3). `fc` holds the probabilities loaded from the
/// saved frame context.
pub fn adapt_coef_probs(fc: &mut FrameContext, counts: &Counts, frame_is_intra: bool, last_frame_was_key: bool) {
    let update_factor = if frame_is_intra {
        112
    } else if last_frame_was_key {
        128
    } else {
        112
    };
    for t in 0..4 {
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..6 {
                    let max_l = if k == 0 { 3 } else { 6 };
                    for l in 0..max_l {
                        let p = &mut fc.coef[t][i][j][k][l];
                        merge_probs(&SMALL_TOKEN_TREE, 2, p, &counts.token[t][i][j][k][l], 24, update_factor);
                        merge_probs(&BINARY_TREE, 0, p, &counts.more_coefs[t][i][j][k][l], 24, update_factor);
                    }
                }
            }
        }
    }
}

/// Non-coefficient probability adaptation (8.4.4).
pub fn adapt_noncoef_probs(fc: &mut FrameContext, c: &Counts, interp_switchable: bool, tx_select: bool, allow_hp: bool) {
    for i in 0..4 {
        adapt_prob(&mut fc.is_inter[i], &c.is_inter[i]);
    }
    for i in 0..5 {
        adapt_prob(&mut fc.comp_mode[i], &c.comp_mode[i]);
        adapt_prob(&mut fc.comp_ref[i], &c.comp_ref[i]);
        for j in 0..2 {
            adapt_prob(&mut fc.single_ref[i][j], &c.single_ref[i][j]);
        }
    }
    for i in 0..7 {
        adapt_probs(&INTER_MODE_TREE, &mut fc.inter_mode[i], &c.inter_mode[i]);
    }
    for i in 0..4 {
        adapt_probs(&INTRA_MODE_TREE, &mut fc.y_mode[i], &c.intra_mode[i]);
    }
    for i in 0..10 {
        adapt_probs(&INTRA_MODE_TREE, &mut fc.uv_mode[i], &c.uv_mode[i]);
    }
    for i in 0..16 {
        adapt_probs(&PARTITION_TREE, &mut fc.partition[i], &c.partition[i]);
    }
    for i in 0..3 {
        adapt_prob(&mut fc.skip[i], &c.skip[i]);
    }
    if interp_switchable {
        for i in 0..4 {
            adapt_probs(&INTERP_FILTER_TREE, &mut fc.interp_filter[i], &c.interp_filter[i]);
        }
    }
    if tx_select {
        for i in 0..2 {
            adapt_probs(&TX_SIZE_8_TREE, &mut fc.tx[1][i], &c.tx[1][i]);
            adapt_probs(&TX_SIZE_16_TREE, &mut fc.tx[2][i], &c.tx[2][i]);
            adapt_probs(&TX_SIZE_32_TREE, &mut fc.tx[3][i], &c.tx[3][i]);
        }
    }
    adapt_probs(&MV_JOINT_TREE, &mut fc.mv_joint, &c.mv_joint);
    for i in 0..2 {
        adapt_prob(&mut fc.mv_sign[i], &c.mv_sign[i]);
        adapt_probs(&MV_CLASS_TREE, &mut fc.mv_class[i], &c.mv_class[i]);
        adapt_prob(&mut fc.mv_class0_bit[i], &c.mv_class0_bit[i]);
        for j in 0..10 {
            adapt_prob(&mut fc.mv_bits[i][j], &c.mv_bits[i][j]);
        }
        for j in 0..2 {
            adapt_probs(&MV_FR_TREE, &mut fc.mv_class0_fr[i][j], &c.mv_class0_fr[i][j]);
        }
        adapt_probs(&MV_FR_TREE, &mut fc.mv_fr[i], &c.mv_fr[i]);
        if allow_hp {
            adapt_prob(&mut fc.mv_class0_hp[i], &c.mv_class0_hp[i]);
            adapt_prob(&mut fc.mv_hp[i], &c.mv_hp[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prob_identity_without_counts() {
        for p in 1..=255u8 {
            assert_eq!(merge_prob(p, 0, 0, 20, 128), p);
        }
        // Saturated counts move halfway towards the observed probability.
        assert_eq!(merge_prob(128, 20, 0, 20, 128), 192);
    }

    #[test]
    fn defaults_load() {
        let fc = FrameContext::default();
        assert_eq!(fc.coef[0][0][0][0][0], [195, 29, 183]);
        assert_eq!(fc.tx[3][1], [5, 52, 13]);
        assert_eq!(fc.mv_class0_fr[1][1], [96, 112, 64]);
    }
}

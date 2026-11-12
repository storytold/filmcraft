//! CELT layer encoder (RFC 6716 §5.3): one 20 ms long-block frame per call.
//!
//! Every side decision the decoder reads is made here in the same order and under the same budget
//! tests as [`super::CeltDecoder`]'s `decode_frame`, and the allocation and band-shape coding run
//! through the decoder's own code ([`compute_allocation`], [`quant_all_bands`]) with a
//! [`RangeEncoder`]. Fixed choices: no post-filter, no transient (long MDCT only), no TF changes,
//! spreading chosen from the bit rate, default allocation trim, no dynamic allocation boosts,
//! every band kept,
//! intensity stereo chosen from the bit rate, no dual stereo, no anti-collapse.

use super::bands::{QuantAllBandsInput, quant_all_bands};
use super::energy::{CoarseEncode, FinaliseEncode, quant_coarse_energy, quant_energy_finalise, quant_fine_energy};
use super::mdct::ImdctScratch;
use super::rate::{AllocationInput, Mode, compute_allocation};
use super::tables::{E_MEANS, EBANDS, MAX_LM, NB_EBANDS, OVERLAP, PREEMPH, SPREAD_ICDF, TF_SELECT_TABLE, TRIM_ICDF};
use crate::range::{BITRES, RangeEncoder};
use crate::{Error, Result};

/// Samples per channel in one frame (20 ms at 48 kHz).
pub const FRAME: usize = 960;
const LM: usize = 3;
/// Default allocation trim.
const TRIM: usize = 5;

pub struct CeltEncoder {
    m: &'static Mode,
    channels: usize,
    /// The last `OVERLAP` pre-emphasised samples of each channel (the MDCT's look-back).
    hist: Vec<f32>,
    /// The previous scaled input sample of each channel (pre-emphasis memory).
    preemph_mem: [f32; 2],
    old_band_e: [f32; 2 * NB_EBANDS],
    /// The decoder's folding-noise seed: the range at the end of the previous frame.
    rng: u32,
    first: bool,
    work: ImdctScratch,
    block: Vec<f32>,
    coeffs: Vec<f32>,
}

impl CeltEncoder {
    pub fn new(channels: usize) -> Result<CeltEncoder> {
        if !(1..=2).contains(&channels) {
            return Err(Error::InvalidArgument("channels must be 1 or 2"));
        }
        Ok(CeltEncoder {
            m: super::mode(),
            channels,
            hist: vec![0.0; channels * OVERLAP],
            preemph_mem: [0.0; 2],
            old_band_e: [0.0; 2 * NB_EBANDS],
            rng: 0,
            first: true,
            work: ImdctScratch::default(),
            block: Vec::with_capacity(FRAME + OVERLAP),
            coeffs: vec![0.0; FRAME],
        })
    }

    /// Encodes one frame (`FRAME` samples per channel, nominal range ±1) into exactly `nbytes`
    /// bytes (2 to 1275). `intensity` is the first band coded with intensity stereo; `spread` is
    /// the spreading decision (0 none … 3 aggressive).
    pub fn encode_frame(&mut self, pcm: &[&[f32]], nbytes: usize, intensity: usize, spread: usize) -> Result<Vec<u8>> {
        let spread = spread.min(3);
        let c = self.channels;
        if pcm.len() != c || pcm.iter().any(|p| p.len() != FRAME) {
            return Err(Error::InvalidArgument("CELT frame: one 960-sample slice per channel"));
        }
        if !(2..=1275).contains(&nbytes) {
            return Err(Error::InvalidArgument("CELT frame: 2 to 1275 bytes"));
        }
        let n = FRAME;
        let m = self.m;
        let end = NB_EBANDS;
        let coded_bins = (EBANDS[end] as usize) << LM;

        // Pre-emphasis, MDCT, band energies and normalised shapes.
        let mut shapes = vec![0f32; c * n];
        let mut log_e = [0f32; 2 * NB_EBANDS];
        let mut band_amp = [0f32; 2 * NB_EBANDS];
        for ch in 0..c {
            self.block.clear();
            self.block.extend_from_slice(&self.hist[ch * OVERLAP..(ch + 1) * OVERLAP]);
            let mut mem = self.preemph_mem[ch];
            for &v in pcm[ch] {
                let v = if v.is_finite() { v.clamp(-4.0, 4.0) * 32768.0 } else { 0.0 };
                self.block.push(v - PREEMPH * mem);
                mem = v;
            }
            self.preemph_mem[ch] = mem;
            self.hist[ch * OVERLAP..(ch + 1) * OVERLAP].copy_from_slice(&self.block[n..n + OVERLAP]);
            m.mdct[MAX_LM - LM].forward(&self.block, &mut self.coeffs, &m.window, OVERLAP, &mut self.work);
            let x = &mut shapes[ch * n..(ch + 1) * n];
            x[..coded_bins].copy_from_slice(&self.coeffs[..coded_bins]);
            for i in 0..end {
                let (lo, hi) = ((EBANDS[i] as usize) << LM, (EBANDS[i + 1] as usize) << LM);
                let sum: f32 = x[lo..hi].iter().map(|v| v * v).sum();
                let amp = (1e-27 + sum).sqrt();
                band_amp[ch * NB_EBANDS + i] = amp;
                log_e[ch * NB_EBANDS + i] = amp.log2() - E_MEANS[i];
                let g = 1.0 / amp;
                for v in x[lo..hi].iter_mut() {
                    *v *= g;
                }
            }
        }
        if c == 1 {
            for i in 0..NB_EBANDS {
                self.old_band_e[i] = self.old_band_e[i].max(self.old_band_e[NB_EBANDS + i]);
            }
        }

        let mut ec = RangeEncoder::new(nbytes);
        let total = nbytes as i32 * 8;
        // Silence flag, post-filter, transient, intra: the decoder's tests, including where it
        // keeps a stale `tell`.
        let mut tell = ec.tell();
        if tell == 1 {
            ec.bit_logp(false, 15);
        }
        if tell + 16 <= total {
            ec.bit_logp(false, 1);
            tell = ec.tell();
        }
        if tell + 3 <= total {
            ec.bit_logp(false, 3);
            tell = ec.tell();
        }
        let intra = if tell + 3 <= total {
            ec.bit_logp(self.first, 3);
            self.first
        } else {
            false
        };
        quant_coarse_energy(&mut ec, CoarseEncode { start: 0, end, e: &log_e, old_e: &mut self.old_band_e, intra, channels: c, lm: LM });
        tf_encode(&mut ec, end);
        // Without room for the symbol the decoder assumes normal spreading.
        let spread = if ec.tell() + 4 <= total {
            ec.icdf(spread, &SPREAD_ICDF, 5);
            spread
        } else {
            2
        };
        // No dynamic allocation boosts.
        let cap = m.init_caps(LM, c);
        let total_bits = total << BITRES;
        let mut tellf = ec.tell_frac() as i32;
        let dynalloc_logp = 6;
        for &cap_i in cap.iter().take(end) {
            if tellf + (dynalloc_logp << BITRES) < total_bits && cap_i > 0 {
                ec.bit_logp(false, dynalloc_logp as u32);
                tellf = ec.tell_frac() as i32;
            }
        }
        if tellf + (6 << BITRES) <= total_bits {
            ec.icdf(TRIM, &TRIM_ICDF, 7);
        }
        let bits = ((nbytes as i32 * 8) << BITRES) - ec.tell_frac() as i32 - 1;
        let offsets = [0i32; NB_EBANDS];
        let alloc = compute_allocation(AllocationInput {
            mode: m,
            start: 0,
            end,
            offsets: &offsets,
            cap: &cap,
            trim: TRIM as i32,
            total: bits,
            channels: c,
            lm: LM,
            intensity,
            coder: &mut ec,
        });
        quant_fine_energy(&mut ec, 0, end, &log_e, &mut self.old_band_e, &alloc.fine_quant, c);
        let mut collapse_masks = vec![0u32; c * NB_EBANDS];
        {
            let (xa, xb) = shapes.split_at_mut(n);
            let yb = if c == 2 { Some(xb) } else { None };
            let mut seed = self.rng;
            quant_all_bands(QuantAllBandsInput {
                mode: m,
                start: 0,
                end,
                x_buf: xa,
                y_buf: yb,
                collapse_masks: &mut collapse_masks,
                pulses: &alloc.pulses,
                short_blocks: false,
                spread,
                dual_stereo: alloc.dual_stereo,
                intensity: alloc.intensity,
                tf_res: &[0; NB_EBANDS],
                total_bits: (nbytes as i32) * (8 << BITRES),
                balance: alloc.balance,
                coder: &mut ec,
                band_e: &band_amp,
                lm: LM,
                coded_bands: alloc.coded_bands,
                seed: &mut seed,
                disable_inv: c == 1,
            });
        }
        let bits_left = nbytes as i32 * 8 - ec.tell();
        quant_energy_finalise(
            &mut ec,
            FinaliseEncode {
                start: 0,
                end,
                e: &log_e,
                old_e: &mut self.old_band_e,
                fine_quant: &alloc.fine_quant,
                fine_priority: &alloc.fine_priority,
                bits_left,
                channels: c,
            },
        );
        if c == 1 {
            let (a, b) = self.old_band_e.split_at_mut(NB_EBANDS);
            b.copy_from_slice(a);
        }
        self.rng = ec.rng();
        self.first = false;
        ec.done()
    }
}

/// `tf_encode` for a long-block frame with no TF changes: the decoder's reads, all zero.
fn tf_encode(ec: &mut RangeEncoder, end: usize) {
    let mut budget = ec.storage() as i32 * 8;
    let mut tell = ec.tell();
    let mut logp = 4;
    let tf_select_rsv = LM > 0 && tell + logp < budget;
    budget -= tf_select_rsv as i32;
    for _ in 0..end {
        if tell + logp <= budget {
            ec.bit_logp(false, logp as u32);
            tell = ec.tell();
        }
        logp = 5;
    }
    // With no change the two TF selections agree (TF_SELECT_TABLE[LM][0] == [2]), so no
    // tf_select bit is coded.
}

const _: () = assert!(TF_SELECT_TABLE[LM][0] == TF_SELECT_TABLE[LM][2]);

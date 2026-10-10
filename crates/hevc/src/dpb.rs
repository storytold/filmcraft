//! Decoded picture buffer: POC derivation (8.3.1), reference picture set (8.3.2), missing
//! reference generation (8.3.3), reference picture lists (8.3.4) and output / bumping (C.5.2).
//!
//! The buffer only does bookkeeping on picture headers; what a picture is (`F`: the software
//! decoder's [`crate::picture::FrameRef`], a hardware decoder's surface) is up to the decoder, so
//! hardware decoders driven by the same headers (VA-API) share this logic and output order.

use crate::error::{Result, ensure};
use crate::slice::SliceHeader;
use std::sync::Arc;

/// Output metadata travelling with a picture.
#[derive(Clone, Debug)]
pub struct OutputMeta {
    pub pts: i64,
    pub key: bool,
    pub crop: (u32, u32, u32, u32),
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub sar: (u16, u16),
    pub bit_depth: u32,
    /// Decoded in draft mode (no in-loop filters).
    pub draft: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marking {
    Unused,
    Short,
    Long,
}

pub struct Entry<F> {
    pub frame: F,
    pub poc: i32,
    pub marking: Marking,
    pub needed_for_output: bool,
    pub latency: u32,
    pub meta: Arc<OutputMeta>,
}

/// A picture leaving the DPB for output.
pub struct Output<F> {
    pub frame: F,
    pub poc: i32,
    pub meta: Arc<OutputMeta>,
}

/// A reference picture of the current picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref<F> {
    pub frame: F,
    pub poc: i32,
    pub long_term: bool,
}

/// Reference picture set of the current picture (8.3.2) as DPB indices / generated frames.
pub struct RefPicSet<F> {
    pub st_curr_before: Vec<Ref<F>>,
    pub st_curr_after: Vec<Ref<F>>,
    pub lt_curr: Vec<Ref<F>>,
}

impl<F> Default for RefPicSet<F> {
    fn default() -> Self {
        Self { st_curr_before: Vec::new(), st_curr_after: Vec::new(), lt_curr: Vec::new() }
    }
}

pub struct Dpb<F> {
    pub entries: Vec<Entry<F>>,
    pub max_dec_pic_buffering: usize,
    pub max_num_reorder: usize,
    /// SpsMaxLatencyPictures (0 = no limit).
    pub max_latency: u32,
}

impl<F> Default for Dpb<F> {
    fn default() -> Self {
        Self { entries: Vec::new(), max_dec_pic_buffering: 0, max_num_reorder: 0, max_latency: 0 }
    }
}

impl<F: Clone> Dpb<F> {
    fn bump(&mut self, outs: &mut Vec<Output<F>>) -> bool {
        let Some(i) = (0..self.entries.len()).filter(|&i| self.entries[i].needed_for_output).min_by_key(|&i| self.entries[i].poc) else { return false };
        let e = &mut self.entries[i];
        e.needed_for_output = false;
        outs.push(Output { frame: e.frame.clone(), poc: e.poc, meta: e.meta.clone() });
        self.remove_unused();
        true
    }

    fn remove_unused(&mut self) {
        self.entries.retain(|e| e.needed_for_output || e.marking != Marking::Unused);
    }

    fn num_output(&self) -> usize {
        self.entries.iter().filter(|e| e.needed_for_output).count()
    }

    fn latency_exceeded(&self) -> bool {
        self.max_latency != 0 && self.entries.iter().any(|e| e.needed_for_output && e.latency >= self.max_latency)
    }

    /// Output everything (end of stream or IRAP without no_output_of_prior_pics).
    pub fn flush(&mut self, outs: &mut Vec<Output<F>>) {
        while self.bump(outs) {}
        for e in &mut self.entries {
            e.marking = Marking::Unused;
        }
        self.entries.clear();
    }

    /// Discard everything without output.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// C.5.2.2: removal of pictures before decoding the current (non-IRAP-reset) picture, after the RPS
    /// has been applied.
    pub fn bump_before_decode(&mut self, outs: &mut Vec<Output<F>>) {
        self.remove_unused();
        loop {
            let full = self.entries.len() >= self.max_dec_pic_buffering.max(1);
            if self.num_output() > self.max_num_reorder || self.latency_exceeded() || full {
                if !self.bump(outs) {
                    // DPB full of reference pictures that are not needed for output: nothing to do
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// C.5.2.3: insert the current picture and apply "additional bumping".
    pub fn insert(&mut self, frame: F, poc: i32, output: bool, meta: Arc<OutputMeta>, outs: &mut Vec<Output<F>>) {
        for e in &mut self.entries {
            if e.needed_for_output {
                e.latency += 1;
            }
        }
        self.entries.push(Entry { frame, poc, marking: Marking::Short, needed_for_output: output, latency: 0, meta });
        while self.num_output() > self.max_num_reorder || self.latency_exceeded() {
            if !self.bump(outs) {
                break;
            }
        }
    }

    /// Apply the RPS of the current picture (8.3.2): mark pictures, find (or generate) references.
    /// `make_missing` creates a replacement frame for a missing reference picture with the given POC.
    pub fn apply_rps(
        &mut self,
        sh: &SliceHeader,
        poc: i32,
        max_poc_lsb: i32,
        irap_no_rasl: bool,
        make_missing: &mut dyn FnMut(i32) -> F,
    ) -> Result<RefPicSet<F>> {
        let mut set = RefPicSet::default();
        if sh.nal.is_irap() && irap_no_rasl {
            for e in &mut self.entries {
                e.marking = Marking::Unused;
            }
        }
        if sh.nal.is_idr() {
            for e in &mut self.entries {
                e.marking = Marking::Unused;
            }
            return Ok(set);
        }
        // Long-term entries first (they may refer to any reference picture).
        let mut keep = vec![false; self.entries.len()];
        let mut lt_idx: Vec<(Option<usize>, i32, bool, bool)> = Vec::new(); // (entry, poc, used, msb_present)
        for &(lsb, used, msb_present, cycle) in &sh.lt {
            let mut p = lsb as i32;
            if msb_present {
                p += poc - (cycle as i32).wrapping_mul(max_poc_lsb) - (poc & (max_poc_lsb - 1));
            }
            let found =
                self.entries.iter().position(|e| e.marking != Marking::Unused && if msb_present { e.poc == p } else { (e.poc & (max_poc_lsb - 1)) == p });
            lt_idx.push((found, p, used, msb_present));
        }
        // Short-term entries.
        let mut st: Vec<(Option<usize>, i32, bool, bool)> = Vec::new(); // (entry, poc, used, before)
        for &(d, used) in &sh.st_rps.s0 {
            let p = poc + d;
            let found = self.entries.iter().position(|e| e.marking == Marking::Short && e.poc == p);
            st.push((found, p, used, true));
        }
        for &(d, used) in &sh.st_rps.s1 {
            let p = poc + d;
            let found = self.entries.iter().position(|e| e.marking == Marking::Short && e.poc == p);
            st.push((found, p, used, false));
        }
        for &(f, _, _, _) in lt_idx.iter() {
            if let Some(i) = f {
                keep[i] = true;
            }
        }
        for &(f, _, _, _) in st.iter() {
            if let Some(i) = f {
                keep[i] = true;
            }
        }
        for (i, e) in self.entries.iter_mut().enumerate() {
            if !keep[i] {
                e.marking = Marking::Unused;
            }
        }
        for &(f, _, _, _) in &lt_idx {
            if let Some(i) = f {
                self.entries[i].marking = Marking::Long;
            }
        }
        let mut make = |found: Option<usize>, p: i32, lt: bool, entries: &mut Vec<Entry<F>>| -> Ref<F> {
            match found {
                Some(i) => Ref { frame: entries[i].frame.clone(), poc: entries[i].poc, long_term: lt },
                None => {
                    // 8.3.3: generate an unavailable reference picture
                    let frame = make_missing(p);
                    Ref { frame, poc: p, long_term: lt }
                }
            }
        };
        for &(f, p, used, before) in &st {
            if !used {
                continue;
            }
            let r = make(f, p, false, &mut self.entries);
            if before {
                set.st_curr_before.push(r);
            } else {
                set.st_curr_after.push(r);
            }
        }
        for &(f, p, used, _) in &lt_idx {
            if used {
                let r = make(f, p, true, &mut self.entries);
                set.lt_curr.push(r);
            }
        }
        Ok(set)
    }
}

/// Build RefPicList0/1 (8.3.4).
pub fn build_ref_lists<F: Clone>(sh: &SliceHeader, rps: &RefPicSet<F>) -> Result<[Vec<Ref<F>>; 2]> {
    let mut lists: [Vec<Ref<F>>; 2] = [Vec::new(), Vec::new()];
    if sh.is_intra() {
        return Ok(lists);
    }
    let total = rps.st_curr_before.len() + rps.st_curr_after.len() + rps.lt_curr.len();
    ensure!(total > 0, "no reference pictures for an inter slice");
    for l in 0..if sh.is_b() { 2 } else { 1 } {
        let n = (sh.num_ref_idx[l] as usize).max(total);
        let mut temp = Vec::with_capacity(n);
        let (first, second) = if l == 0 { (&rps.st_curr_before, &rps.st_curr_after) } else { (&rps.st_curr_after, &rps.st_curr_before) };
        while temp.len() < n {
            for r in first.iter().chain(second.iter()).chain(rps.lt_curr.iter()) {
                if temp.len() < n {
                    temp.push(r.clone());
                }
            }
        }
        for i in 0..sh.num_ref_idx[l] as usize {
            let idx = match &sh.list_entry[l] {
                Some(e) => e[i] as usize,
                None => i,
            };
            ensure!(idx < temp.len(), "list_entry out of range");
            lists[l].push(temp[idx].clone());
        }
    }
    Ok(lists)
}

/// POC state (8.3.1).
#[derive(Default, Clone, Copy)]
pub struct PocState {
    pub prev_tid0_poc: i32,
}

impl PocState {
    pub fn compute(&self, sh: &SliceHeader, max_poc_lsb: i32, irap_no_rasl: bool) -> i32 {
        let lsb = sh.poc_lsb as i32;
        let msb = if sh.nal.is_irap() && irap_no_rasl {
            0
        } else {
            let prev_lsb = self.prev_tid0_poc & (max_poc_lsb - 1);
            let prev_msb = self.prev_tid0_poc - prev_lsb;
            if lsb < prev_lsb && prev_lsb - lsb >= max_poc_lsb / 2 {
                prev_msb + max_poc_lsb
            } else if lsb > prev_lsb && lsb - prev_lsb > max_poc_lsb / 2 {
                prev_msb - max_poc_lsb
            } else {
                prev_msb
            }
        };
        msb + lsb
    }

    /// Update after decoding a picture with the given header and POC.
    pub fn update(&mut self, sh: &SliceHeader, poc: i32) {
        if sh.nal.temporal_id == 0 && !sh.nal.is_rasl() && !sh.nal.is_radl() && !sh.nal.is_sub_layer_non_ref() {
            self.prev_tid0_poc = poc;
        }
    }
}

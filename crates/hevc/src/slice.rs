//! NAL unit header (7.3.1.2) and slice segment header (7.3.6).

use crate::error::{Result, ensure, invalid};
use crate::params::{Pps, Sps, StRps};
use filmcraft_bitstream::BitReader;

pub mod nal_type {
    pub const TRAIL_N: u8 = 0;
    pub const RADL_N: u8 = 6;
    pub const RADL_R: u8 = 7;
    pub const RASL_N: u8 = 8;
    pub const RASL_R: u8 = 9;
    pub const RSV_VCL_N14: u8 = 14;
    pub const BLA_W_LP: u8 = 16;
    pub const BLA_W_RADL: u8 = 17;
    pub const BLA_N_LP: u8 = 18;
    pub const IDR_W_RADL: u8 = 19;
    pub const IDR_N_LP: u8 = 20;
    pub const CRA: u8 = 21;
    pub const RSV_IRAP_23: u8 = 23;
    pub const VPS: u8 = 32;
    pub const SPS: u8 = 33;
    pub const PPS: u8 = 34;
    pub const AUD: u8 = 35;
    pub const EOS: u8 = 36;
    pub const EOB: u8 = 37;
    pub const FD: u8 = 38;
    pub const PREFIX_SEI: u8 = 39;
    pub const SUFFIX_SEI: u8 = 40;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NalHeader {
    pub nal_type: u8,
    pub layer_id: u8,
    pub temporal_id: u8,
}

impl NalHeader {
    pub fn parse(b: &[u8]) -> Result<NalHeader> {
        ensure!(b.len() >= 2, "NAL unit too short");
        ensure!(b[0] & 0x80 == 0, "forbidden_zero_bit set");
        let tid = b[1] & 7;
        ensure!(tid != 0, "nuh_temporal_id_plus1 is 0");
        Ok(NalHeader { nal_type: (b[0] >> 1) & 0x3f, layer_id: ((b[0] & 1) << 5) | (b[1] >> 3), temporal_id: tid - 1 })
    }
    pub fn is_vcl(&self) -> bool {
        self.nal_type < 32
    }
    pub fn is_irap(&self) -> bool {
        (nal_type::BLA_W_LP..=nal_type::RSV_IRAP_23).contains(&self.nal_type)
    }
    pub fn is_idr(&self) -> bool {
        self.nal_type == nal_type::IDR_W_RADL || self.nal_type == nal_type::IDR_N_LP
    }
    pub fn is_bla(&self) -> bool {
        (nal_type::BLA_W_LP..=nal_type::BLA_N_LP).contains(&self.nal_type)
    }
    pub fn is_cra(&self) -> bool {
        self.nal_type == nal_type::CRA
    }
    pub fn is_rasl(&self) -> bool {
        self.nal_type == nal_type::RASL_N || self.nal_type == nal_type::RASL_R
    }
    pub fn is_radl(&self) -> bool {
        self.nal_type == nal_type::RADL_N || self.nal_type == nal_type::RADL_R
    }
    /// Sub-layer non-reference picture (TRAIL_N, TSA_N, ..., RSV_VCL_N14).
    pub fn is_sub_layer_non_ref(&self) -> bool {
        self.nal_type <= nal_type::RSV_VCL_N14 && self.nal_type.is_multiple_of(2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceType {
    B = 0,
    P = 1,
    I = 2,
}

/// Explicit weighted prediction parameters for one reference (7.4.7.3), already derived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WpEntry {
    /// LumaWeightLX, luma_offset_lX (8-bit units).
    pub luma: (i32, i32),
    pub luma_flag: bool,
    /// ChromaWeightLX, ChromaOffsetLX (8-bit units) for Cb and Cr.
    pub chroma: [(i32, i32); 2],
    pub chroma_flag: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PredWeightTable {
    pub luma_log2_denom: u32,
    pub chroma_log2_denom: u32,
    pub l: [Vec<WpEntry>; 2],
}

#[derive(Clone, Debug)]
pub struct SliceHeader {
    pub nal: NalHeader,
    pub first_slice_segment_in_pic: bool,
    pub no_output_of_prior_pics: bool,
    pub pps_id: u32,
    pub dependent: bool,
    pub segment_address: u32,
    pub slice_type: SliceType,
    pub pic_output: bool,
    pub poc_lsb: u32,
    /// The short-term RPS used by this picture (from the SPS or the header).
    pub st_rps: StRps,
    pub st_rps_bits: usize,
    /// Long-term entries: (PocLsbLt, UsedByCurrPicLt, delta_poc_msb_present, DeltaPocMsbCycleLt).
    pub lt: Vec<(u32, bool, bool, u32)>,
    pub temporal_mvp: bool,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub num_ref_idx: [u32; 2],
    pub list_entry: [Option<Vec<u32>>; 2],
    pub mvd_l1_zero: bool,
    pub cabac_init_flag: bool,
    pub collocated_from_l0: bool,
    pub collocated_ref_idx: u32,
    pub pwt: Option<PredWeightTable>,
    pub max_num_merge_cand: u32,
    pub qp_delta: i32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub deblocking_disabled: bool,
    pub beta_offset_div2: i32,
    pub tc_offset_div2: i32,
    pub loop_filter_across_slices: bool,
    pub entry_points: Vec<u32>,
    /// Byte offset of slice_segment_data() in the RBSP.
    pub data_offset: usize,
}

fn ceil_log2(n: u32) -> u32 {
    if n <= 1 { 0 } else { 32 - (n - 1).leading_zeros() }
}

impl SliceHeader {
    pub fn is_intra(&self) -> bool {
        self.slice_type == SliceType::I
    }
    pub fn is_b(&self) -> bool {
        self.slice_type == SliceType::B
    }

    /// NumPicTotalCurr (7-55), without pps_curr_pic_ref.
    pub fn num_pic_total_curr(&self) -> u32 {
        let st = self.st_rps.s0.iter().chain(self.st_rps.s1.iter()).filter(|e| e.1).count();
        let lt = self.lt.iter().filter(|e| e.1).count();
        (st + lt) as u32
    }

    /// Parse a slice segment header. `prev` is the header of the preceding independent slice segment
    /// of the same picture (needed for dependent slice segments).
    pub fn parse(
        rbsp: &[u8],
        nal: NalHeader,
        lookup: impl FnOnce(u32) -> Result<(std::sync::Arc<Pps>, std::sync::Arc<Sps>)>,
        prev: Option<&SliceHeader>,
    ) -> Result<(SliceHeader, std::sync::Arc<Pps>, std::sync::Arc<Sps>)> {
        let mut r = BitReader::new(rbsp);
        let first = r.read_flag()?;
        let no_output_of_prior_pics = if nal.is_irap() { r.read_flag()? } else { false };
        let pps_id = r.read_ue()?;
        ensure!(pps_id < 64, "slice_pic_parameter_set_id out of range");
        let (pps, sps) = lookup(pps_id)?;
        let mut dependent = false;
        let mut segment_address = 0;
        if !first {
            if pps.dependent_slice_segments_enabled {
                dependent = r.read_flag()?;
            }
            let pic_size = sps.pic_width_in_ctbs() * sps.pic_height_in_ctbs();
            segment_address = r.read_bits(ceil_log2(pic_size))?;
            ensure!(segment_address < pic_size, "slice_segment_address out of range");
        }
        let mut sh = if dependent {
            let Some(p) = prev else { return invalid("dependent slice segment without a preceding slice") };
            let mut s = p.clone();
            s.nal = nal;
            s
        } else {
            SliceHeader::with_pps_defaults(nal, first, no_output_of_prior_pics, pps_id, segment_address, &pps)
        };
        sh.first_slice_segment_in_pic = first;
        sh.no_output_of_prior_pics = no_output_of_prior_pics;
        sh.pps_id = pps_id;
        sh.dependent = dependent;
        sh.segment_address = segment_address;
        if !dependent {
            parse_independent_fields(&mut r, &mut sh, nal, &pps, &sps)?;
        }
        parse_entry_points_and_extension(&mut r, &mut sh, &pps, &sps)?;
        // byte_alignment(): alignment_bit_equal_to_one then zeros
        ensure!(r.read_flag()?, "missing alignment_bit_equal_to_one");
        r.byte_align();
        sh.data_offset = r.position() / 8;
        ensure!(sh.data_offset <= rbsp.len(), "slice header runs past the NAL unit");
        Ok((sh, pps, sps))
    }
}

impl SliceHeader {
    /// A fresh independent slice header: syntax elements not yet parsed take their inferred
    /// values, the deblocking parameters those of the PPS.
    fn with_pps_defaults(nal: NalHeader, first: bool, no_output_of_prior_pics: bool, pps_id: u32, segment_address: u32, pps: &Pps) -> SliceHeader {
        SliceHeader {
            nal,
            first_slice_segment_in_pic: first,
            no_output_of_prior_pics,
            pps_id,
            dependent: false,
            segment_address,
            slice_type: SliceType::I,
            pic_output: true,
            poc_lsb: 0,
            st_rps: StRps::default(),
            st_rps_bits: 0,
            lt: Vec::new(),
            temporal_mvp: false,
            sao_luma: false,
            sao_chroma: false,
            num_ref_idx: [0, 0],
            list_entry: [None, None],
            mvd_l1_zero: false,
            cabac_init_flag: false,
            collocated_from_l0: true,
            collocated_ref_idx: 0,
            pwt: None,
            max_num_merge_cand: 5,
            qp_delta: 0,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            deblocking_disabled: pps.deblocking_disabled,
            beta_offset_div2: pps.beta_offset_div2,
            tc_offset_div2: pps.tc_offset_div2,
            loop_filter_across_slices: pps.loop_filter_across_slices,
            entry_points: Vec::new(),
            data_offset: 0,
        }
    }
}

/// Fields present only in independent slice segments, up to the entry points.
fn parse_independent_fields(r: &mut BitReader, sh: &mut SliceHeader, nal: NalHeader, pps: &Pps, sps: &Sps) -> Result<()> {
    r.skip(pps.num_extra_slice_header_bits as usize)?;
    let st = r.read_ue()?;
    sh.slice_type = match st {
        0 => SliceType::B,
        1 => SliceType::P,
        2 => SliceType::I,
        _ => return invalid(format!("slice_type {st}")),
    };
    if nal.is_irap() {
        ensure!(sh.slice_type == SliceType::I, "IRAP picture with inter slice");
    }
    if pps.output_flag_present {
        sh.pic_output = r.read_flag()?;
    }
    if sps.separate_colour_plane {
        r.skip(2)?;
    }
    if !nal.is_idr() {
        parse_reference_pictures(r, sh, sps)?;
    }
    if sps.sao {
        sh.sao_luma = r.read_flag()?;
        if sps.chroma_array_type() != 0 {
            sh.sao_chroma = r.read_flag()?;
        }
    }
    if sh.slice_type != SliceType::I {
        parse_inter_slice_params(r, sh, pps, sps)?;
    }
    parse_qp_and_loop_filter(r, sh, pps, sps)
}

/// POC LSB, short-term and long-term reference picture sets and the temporal MVP flag.
fn parse_reference_pictures(r: &mut BitReader, sh: &mut SliceHeader, sps: &Sps) -> Result<()> {
    sh.poc_lsb = r.read_bits(sps.log2_max_poc_lsb)?;
    let from_sps = r.read_flag()?;
    if !from_sps {
        let start = r.position();
        sh.st_rps = StRps::parse(r, sps.st_rps.len(), &sps.st_rps, sps.st_rps.len())?;
        sh.st_rps_bits = r.position() - start;
    } else {
        ensure!(!sps.st_rps.is_empty(), "no short-term RPS in the SPS");
        let idx = if sps.st_rps.len() > 1 { r.read_bits(ceil_log2(sps.st_rps.len() as u32))? as usize } else { 0 };
        ensure!(idx < sps.st_rps.len(), "short_term_ref_pic_set_idx out of range");
        sh.st_rps = sps.st_rps[idx].clone();
    }
    if sps.long_term_refs_present {
        parse_long_term_refs(r, sh, sps)?;
    }
    if sps.temporal_mvp {
        sh.temporal_mvp = r.read_flag()?;
    }
    Ok(())
}

/// Long-term reference pictures (num_long_term_sps .. delta_poc_msb_cycle_lt).
fn parse_long_term_refs(r: &mut BitReader, sh: &mut SliceHeader, sps: &Sps) -> Result<()> {
    let num_lt_sps = if !sps.lt_ref_pics.is_empty() { r.read_ue()? } else { 0 };
    ensure!(num_lt_sps as usize <= sps.lt_ref_pics.len(), "num_long_term_sps out of range");
    let num_lt_pics = r.read_ue()?;
    ensure!(num_lt_sps + num_lt_pics <= 32, "too many long-term pictures");
    let mut msb_acc = 0u32;
    for i in 0..num_lt_sps + num_lt_pics {
        let (lsb, used) = if i < num_lt_sps {
            let idx = if sps.lt_ref_pics.len() > 1 { r.read_bits(ceil_log2(sps.lt_ref_pics.len() as u32))? } else { 0 };
            ensure!((idx as usize) < sps.lt_ref_pics.len(), "lt_idx_sps out of range");
            sps.lt_ref_pics[idx as usize]
        } else {
            (r.read_bits(sps.log2_max_poc_lsb)?, r.read_flag()?)
        };
        let msb_present = r.read_flag()?;
        let cycle = if msb_present { r.read_ue()? } else { 0 };
        // DeltaPocMsbCycleLt accumulates within each of the two groups (7-52)
        if i == 0 || i == num_lt_sps {
            msb_acc = cycle;
        } else {
            msb_acc = msb_acc.wrapping_add(cycle);
        }
        sh.lt.push((lsb, used, msb_present, msb_acc));
    }
    Ok(())
}

/// ref_pic_lists_modification: list_entry_lX for each list.
fn parse_list_modification(r: &mut BitReader, sh: &mut SliceHeader, total: u32) -> Result<()> {
    let bits = ceil_log2(total);
    for l in 0..if sh.is_b() { 2 } else { 1 } {
        if r.read_flag()? {
            let mut e = Vec::new();
            for _ in 0..sh.num_ref_idx[l] {
                let v = r.read_bits(bits)?;
                ensure!(v < total, "list_entry out of range");
                e.push(v);
            }
            sh.list_entry[l] = Some(e);
        }
    }
    Ok(())
}

/// Inter-slice parameters: active reference counts, list modification, collocated picture,
/// weighted prediction and merge candidates.
fn parse_inter_slice_params(r: &mut BitReader, sh: &mut SliceHeader, pps: &Pps, sps: &Sps) -> Result<()> {
    sh.num_ref_idx = [pps.num_ref_idx_l0_default, if sh.is_b() { pps.num_ref_idx_l1_default } else { 0 }];
    if r.read_flag()? {
        sh.num_ref_idx[0] = r.read_ue()? + 1;
        if sh.is_b() {
            sh.num_ref_idx[1] = r.read_ue()? + 1;
        }
    }
    ensure!(sh.num_ref_idx[0] <= 15 && sh.num_ref_idx[1] <= 15, "num_ref_idx_active out of range");
    let total = sh.num_pic_total_curr();
    ensure!(total > 0, "inter slice without reference pictures in the RPS");
    if pps.lists_modification_present && total > 1 {
        parse_list_modification(r, sh, total)?;
    }
    if sh.is_b() {
        sh.mvd_l1_zero = r.read_flag()?;
    }
    if pps.cabac_init_present {
        sh.cabac_init_flag = r.read_flag()?;
    }
    if sh.temporal_mvp {
        if sh.is_b() {
            sh.collocated_from_l0 = r.read_flag()?;
        }
        let l = if sh.collocated_from_l0 { 0 } else { 1 };
        if sh.num_ref_idx[l] > 1 {
            sh.collocated_ref_idx = r.read_ue()?;
            ensure!(sh.collocated_ref_idx < sh.num_ref_idx[l], "collocated_ref_idx out of range");
        }
    }
    if (pps.weighted_pred && sh.slice_type == SliceType::P) || (pps.weighted_bipred && sh.is_b()) {
        sh.pwt = Some(parse_pwt(r, sh, sps)?);
    }
    let five_minus = r.read_ue()?;
    ensure!(five_minus <= 4, "five_minus_max_num_merge_cand out of range");
    sh.max_num_merge_cand = 5 - five_minus;
    Ok(())
}

/// Slice QP delta, chroma QP offsets and deblocking / in-loop filter controls.
fn parse_qp_and_loop_filter(r: &mut BitReader, sh: &mut SliceHeader, pps: &Pps, sps: &Sps) -> Result<()> {
    sh.qp_delta = r.read_se()?;
    let qp = pps.init_qp + sh.qp_delta;
    let qp_bd = 6 * (sps.bit_depth_luma as i32 - 8);
    ensure!((-qp_bd..=51).contains(&qp), "SliceQpY {qp} out of range");
    if pps.slice_chroma_qp_offsets_present {
        sh.cb_qp_offset = r.read_se()?;
        sh.cr_qp_offset = r.read_se()?;
        ensure!((-12..=12).contains(&sh.cb_qp_offset) && (-12..=12).contains(&sh.cr_qp_offset), "slice chroma QP offset out of range");
    }
    let override_flag = if pps.deblocking_override_enabled { r.read_flag()? } else { false };
    if override_flag {
        sh.deblocking_disabled = r.read_flag()?;
        if !sh.deblocking_disabled {
            sh.beta_offset_div2 = r.read_se()?;
            sh.tc_offset_div2 = r.read_se()?;
            ensure!((-6..=6).contains(&sh.beta_offset_div2) && (-6..=6).contains(&sh.tc_offset_div2), "deblocking offsets out of range");
        }
    }
    if pps.loop_filter_across_slices && (sh.sao_luma || sh.sao_chroma || !sh.deblocking_disabled) {
        sh.loop_filter_across_slices = r.read_flag()?;
    }
    Ok(())
}

/// Entry point offsets and the slice header extension.
fn parse_entry_points_and_extension(r: &mut BitReader, sh: &mut SliceHeader, pps: &Pps, sps: &Sps) -> Result<()> {
    sh.entry_points.clear();
    if pps.tiles_enabled || pps.entropy_coding_sync {
        let n = r.read_ue()?;
        ensure!(n <= sps.pic_width_in_ctbs() * sps.pic_height_in_ctbs(), "num_entry_point_offsets out of range");
        if n > 0 {
            let len = r.read_ue()? + 1;
            ensure!(len <= 32, "offset_len_minus1 out of range");
            for _ in 0..n {
                sh.entry_points.push(r.read_bits(len)? + 1);
            }
        }
    }
    if pps.slice_header_extension_present {
        let len = r.read_ue()?;
        ensure!(len <= 256, "slice_segment_header_extension_length out of range");
        r.skip(8 * len as usize)?;
    }
    Ok(())
}

fn parse_pwt(r: &mut BitReader, sh: &SliceHeader, sps: &Sps) -> Result<PredWeightTable> {
    let mut t = PredWeightTable { luma_log2_denom: r.read_ue()?, ..Default::default() };
    ensure!(t.luma_log2_denom <= 7, "luma_log2_weight_denom out of range");
    let chroma = sps.chroma_array_type() != 0;
    if chroma {
        let d = t.luma_log2_denom as i32 + r.read_se()?;
        ensure!((0..=7).contains(&d), "ChromaLog2WeightDenom out of range");
        t.chroma_log2_denom = d as u32;
    }
    let lists = if sh.is_b() { 2 } else { 1 };
    for l in 0..lists {
        let n = sh.num_ref_idx[l] as usize;
        let mut e = vec![WpEntry::default(); n];
        for x in e.iter_mut() {
            x.luma_flag = r.read_flag()?;
        }
        if chroma {
            for x in e.iter_mut() {
                x.chroma_flag = r.read_flag()?;
            }
        }
        for x in e.iter_mut() {
            x.luma = (1 << t.luma_log2_denom, 0);
            if x.luma_flag {
                let dw = r.read_se()?;
                let off = r.read_se()?;
                ensure!((-128..=127).contains(&dw) && (-128..=127).contains(&off), "luma weight out of range");
                x.luma = ((1 << t.luma_log2_denom) + dw, off);
            }
            x.chroma = [(1 << t.chroma_log2_denom, 0); 2];
            if x.chroma_flag {
                let half = 128; // wpOffsetHalfRangeC (no high_precision_offsets)
                for c in 0..2 {
                    let dw = r.read_se()?;
                    let doff = r.read_se()?;
                    ensure!((-128..=127).contains(&dw) && (-4 * half..4 * half).contains(&doff), "chroma weight out of range");
                    let w = (1 << t.chroma_log2_denom) + dw;
                    let off = (half - ((half * w) >> t.chroma_log2_denom) + doff).clamp(-half, half - 1);
                    x.chroma[c] = (w, off);
                }
            }
        }
        t.l[l] = e;
    }
    Ok(t)
}

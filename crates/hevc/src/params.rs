//! Parameter sets: VPS (7.3.2.1), SPS (7.3.2.2), PPS (7.3.2.3), profile/tier/level, scaling lists
//! (7.3.4), short-term reference picture sets (7.3.7) and VUI (E.2.1).

use crate::error::{Result, ensure, unsupported};
use crate::spec_tables::{DEFAULT_SCALING_INTER, DEFAULT_SCALING_INTRA};
use crate::tables::scan_diag;
use filmcraft_bitstream::BitReader;

/// HEVC picture state includes several per-pixel and per-block work buffers. This permits 8K
/// pictures while rejecting dimensions that could exhaust memory during decode.
pub const MAX_LUMA_SAMPLES: u64 = 1 << 26;

fn check_picture_size(width: u32, height: u32) -> Result<()> {
    ensure!(width > 0 && height > 0 && width <= 16888 && height <= 16888, "picture size {width}x{height} out of range");
    ensure!(u64::from(width) * u64::from(height) <= MAX_LUMA_SAMPLES, "picture size {width}x{height} exceeds the decoded-frame budget");
    Ok(())
}

/// General profile / tier / level information.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileTierLevel {
    pub profile_space: u8,
    pub tier: bool,
    pub profile_idc: u8,
    pub compatibility: u32,
    pub progressive_source: bool,
    pub interlaced_source: bool,
    pub level_idc: u8,
}

pub fn parse_ptl(r: &mut BitReader, profile_present: bool, max_sub_layers_minus1: u32) -> Result<ProfileTierLevel> {
    let mut p = ProfileTierLevel::default();
    if profile_present {
        p.profile_space = r.read_bits(2)? as u8;
        p.tier = r.read_flag()?;
        p.profile_idc = r.read_bits(5)? as u8;
        p.compatibility = r.read_bits(32)?;
        p.progressive_source = r.read_flag()?;
        p.interlaced_source = r.read_flag()?;
        r.skip(2)?; // non_packed, frame_only
        r.skip(43)?; // constraint flags / reserved bits (always 43 bits)
        r.skip(1)?; // inbld / reserved
    }
    p.level_idc = r.read_bits(8)? as u8;
    let n = max_sub_layers_minus1 as usize;
    let mut sub_profile = [false; 8];
    let mut sub_level = [false; 8];
    for i in 0..n {
        sub_profile[i] = r.read_flag()?;
        sub_level[i] = r.read_flag()?;
    }
    if n > 0 {
        for _ in n..8 {
            r.skip(2)?;
        }
    }
    for i in 0..n {
        if sub_profile[i] {
            r.skip(2 + 1 + 5 + 32 + 4 + 43 + 1)?;
        }
        if sub_level[i] {
            r.skip(8)?;
        }
    }
    Ok(p)
}

fn skip_sub_layer_hrd(r: &mut BitReader, cpb_cnt: u32, sub_pic: bool) -> Result<()> {
    for _ in 0..cpb_cnt {
        r.read_ue()?;
        r.read_ue()?;
        if sub_pic {
            r.read_ue()?;
            r.read_ue()?;
        }
        r.skip(1)?;
    }
    Ok(())
}

/// hrd_parameters() (E.2.2), skipped.
pub fn skip_hrd(r: &mut BitReader, common: bool, max_sub_layers_minus1: u32) -> Result<()> {
    let (mut nal, mut vcl, mut sub_pic) = (false, false, false);
    if common {
        nal = r.read_flag()?;
        vcl = r.read_flag()?;
        if nal || vcl {
            sub_pic = r.read_flag()?;
            if sub_pic {
                r.skip(8 + 5 + 1 + 5)?;
            }
            r.skip(8)?;
            if sub_pic {
                r.skip(4)?;
            }
            r.skip(15)?;
        }
    }
    for _ in 0..=max_sub_layers_minus1 {
        let fixed_general = r.read_flag()?;
        let fixed_cvs = if !fixed_general { r.read_flag()? } else { true };
        let mut low_delay = false;
        if fixed_cvs {
            r.read_ue()?;
        } else {
            low_delay = r.read_flag()?;
        }
        let mut cpb_cnt = 1;
        if !low_delay {
            cpb_cnt = r.read_ue()? + 1;
            ensure!(cpb_cnt <= 32, "cpb_cnt_minus1 out of range");
        }
        if nal {
            skip_sub_layer_hrd(r, cpb_cnt, sub_pic)?;
        }
        if vcl {
            skip_sub_layer_hrd(r, cpb_cnt, sub_pic)?;
        }
    }
    Ok(())
}

/// Video parameter set (only the fields a single-layer decoder needs).
#[derive(Clone, Debug)]
pub struct Vps {
    pub id: u8,
    pub max_sub_layers_minus1: u32,
    pub ptl: ProfileTierLevel,
}

impl Vps {
    pub fn parse(rbsp: &[u8]) -> Result<Vps> {
        let mut r = BitReader::new(rbsp);
        let id = r.read_bits(4)? as u8;
        r.skip(2 + 6)?;
        let max_sub_layers_minus1 = r.read_bits(3)?;
        r.skip(1 + 16)?;
        let ptl = parse_ptl(&mut r, true, max_sub_layers_minus1)?;
        Ok(Vps { id, max_sub_layers_minus1, ptl })
    }
}

/// VUI fields relevant for output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vui {
    pub sar: (u16, u16),
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub field_seq: bool,
    pub timing: Option<(u32, u32)>,
}

impl Default for Vui {
    fn default() -> Self {
        Vui { sar: (0, 0), full_range: false, colour_primaries: 2, transfer_characteristics: 2, matrix_coefficients: 2, field_seq: false, timing: None }
    }
}

const SAR_TABLE: [(u16, u16); 17] = [
    (0, 0),
    (1, 1),
    (12, 11),
    (10, 11),
    (16, 11),
    (40, 33),
    (24, 11),
    (20, 11),
    (32, 11),
    (80, 33),
    (18, 11),
    (15, 11),
    (64, 33),
    (160, 99),
    (4, 3),
    (3, 2),
    (2, 1),
];

fn parse_vui(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<Vui> {
    let mut v = Vui::default();
    if r.read_flag()? {
        let idc = r.read_bits(8)?;
        if idc == 255 {
            v.sar = (r.read_bits(16)? as u16, r.read_bits(16)? as u16);
        } else if (idc as usize) < SAR_TABLE.len() {
            v.sar = SAR_TABLE[idc as usize];
        }
    }
    if r.read_flag()? {
        r.skip(1)?;
    }
    if r.read_flag()? {
        r.skip(3)?;
        v.full_range = r.read_flag()?;
        if r.read_flag()? {
            v.colour_primaries = r.read_bits(8)? as u8;
            v.transfer_characteristics = r.read_bits(8)? as u8;
            v.matrix_coefficients = r.read_bits(8)? as u8;
        }
    }
    if r.read_flag()? {
        r.read_ue()?;
        r.read_ue()?;
    }
    r.skip(1)?; // neutral_chroma_indication_flag
    v.field_seq = r.read_flag()?;
    r.skip(1)?; // frame_field_info_present_flag
    if r.read_flag()? {
        for _ in 0..4 {
            r.read_ue()?;
        }
    }
    if r.read_flag()? {
        let units = r.read_bits(32)?;
        let scale = r.read_bits(32)?;
        v.timing = Some((units, scale));
        if r.read_flag()? {
            r.read_ue()?;
        }
        if r.read_flag()? {
            skip_hrd(r, true, max_sub_layers_minus1)?;
        }
    }
    if r.read_flag()? {
        r.skip(3)?;
        for _ in 0..5 {
            r.read_ue()?;
        }
    }
    Ok(v)
}

/// Scaling list data (7.3.4) with prediction resolved: lists in up-right diagonal coefficient order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalingList {
    /// [sizeId][matrixId][i]; sizeId 0 uses 16 entries.
    pub lists: [[[u8; 64]; 6]; 4],
    /// scaling_list_dc_coef for sizeId 2, 3.
    pub dc: [[u8; 6]; 2],
}

impl ScalingList {
    /// Default lists (Tables 7-5, 7-6).
    pub fn default_lists() -> Self {
        let mut lists = [[[16u8; 64]; 6]; 4];
        for size in 1..4 {
            for m in 0..6 {
                lists[size][m] = if m < 3 { DEFAULT_SCALING_INTRA } else { DEFAULT_SCALING_INTER };
            }
        }
        ScalingList { lists, dc: [[16; 6]; 2] }
    }

    pub fn parse(r: &mut BitReader) -> Result<Self> {
        let mut sl = Self::default_lists();
        for size in 0..4 {
            let step = if size == 3 { 3 } else { 1 };
            let mut m = 0;
            while m < 6 {
                let coef_num = if size == 0 { 16 } else { 64 };
                if !r.read_flag()? {
                    let delta = r.read_ue()? as usize * step;
                    ensure!(delta <= m, "scaling_list_pred_matrix_id_delta out of range");
                    if delta == 0 {
                        // default list
                        let d = Self::default_lists();
                        sl.lists[size][m] = d.lists[size][m];
                        if size > 1 {
                            sl.dc[size - 2][m] = 16;
                        }
                    } else {
                        let rm = m - delta;
                        sl.lists[size][m] = sl.lists[size][rm];
                        if size > 1 {
                            sl.dc[size - 2][m] = sl.dc[size - 2][rm];
                        }
                    }
                } else {
                    let mut next: i32 = 8;
                    if size > 1 {
                        let dc = r.read_se()?;
                        ensure!((-7..=247).contains(&dc), "scaling_list_dc_coef_minus8 out of range");
                        next = dc + 8;
                        sl.dc[size - 2][m] = next as u8;
                    }
                    for i in 0..coef_num {
                        let d = r.read_se()?;
                        ensure!((-128..=127).contains(&d), "scaling_list_delta_coef out of range");
                        next = (next + d + 256).rem_euclid(256);
                        sl.lists[size][m][i] = next as u8;
                    }
                }
                m += step;
            }
        }
        // 32x32 chroma lists (only used for ChromaArrayType 3) are copied from the 16x16 ones.
        for m in [1, 2, 4, 5] {
            sl.lists[3][m] = sl.lists[2][m];
            sl.dc[1][m] = sl.dc[0][m];
        }
        Ok(sl)
    }

    /// ScalingFactor for sizeId (log2 size = sizeId + 2) and matrixId as a row-major [y][x] matrix
    /// (7-44..7-48).
    pub fn factor(&self, size_id: usize, matrix_id: usize) -> Vec<u8> {
        let n = 4usize << size_id;
        let mut out = vec![0u8; n * n];
        if size_id == 0 {
            let scan = scan_diag(2);
            for (i, &(x, y)) in scan.iter().enumerate() {
                out[y as usize * 4 + x as usize] = self.lists[0][matrix_id][i];
            }
            return out;
        }
        let rep = n / 8;
        let scan = scan_diag(3);
        for (i, &(x, y)) in scan.iter().enumerate() {
            let v = self.lists[size_id][matrix_id][i];
            for j in 0..rep {
                for k in 0..rep {
                    out[(y as usize * rep + j) * n + x as usize * rep + k] = v;
                }
            }
        }
        if size_id >= 2 {
            out[0] = self.dc[size_id - 2][matrix_id];
        }
        out
    }
}

/// A short-term reference picture set (7.4.8), resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StRps {
    /// DeltaPocS0 (negative, decreasing) with UsedByCurrPicS0.
    pub s0: Vec<(i32, bool)>,
    /// DeltaPocS1 (positive, increasing) with UsedByCurrPicS1.
    pub s1: Vec<(i32, bool)>,
}

impl StRps {
    pub fn num_delta_pocs(&self) -> usize {
        self.s0.len() + self.s1.len()
    }

    /// st_ref_pic_set(idx) with the previously parsed candidate sets `sets` (idx == sets.len() for a
    /// slice-header RPS).
    pub fn parse(r: &mut BitReader, idx: usize, sets: &[StRps], num_sets: usize) -> Result<StRps> {
        let inter = if idx != 0 { r.read_flag()? } else { false };
        if inter {
            let delta_idx = if idx == num_sets { r.read_ue()? as usize + 1 } else { 1 };
            ensure!(delta_idx <= idx, "delta_idx_minus1 out of range");
            let rf = &sets[idx - delta_idx];
            let sign = r.read_flag()?;
            let abs = r.read_ue()? as i32 + 1;
            ensure!(abs <= 1 << 15, "abs_delta_rps_minus1 out of range");
            let delta_rps = if sign { -abs } else { abs };
            let n = rf.num_delta_pocs();
            let mut used = vec![false; n + 1];
            let mut use_delta = vec![true; n + 1];
            for j in 0..=n {
                used[j] = r.read_flag()?;
                if !used[j] {
                    use_delta[j] = r.read_flag()?;
                }
            }
            let nneg = rf.s0.len();
            let mut out = StRps::default();
            for j in (0..rf.s1.len()).rev() {
                let d = rf.s1[j].0 + delta_rps;
                if d < 0 && use_delta[nneg + j] {
                    out.s0.push((d, used[nneg + j]));
                }
            }
            if delta_rps < 0 && use_delta[n] {
                out.s0.push((delta_rps, used[n]));
            }
            for j in 0..nneg {
                let d = rf.s0[j].0 + delta_rps;
                if d < 0 && use_delta[j] {
                    out.s0.push((d, used[j]));
                }
            }
            for j in (0..nneg).rev() {
                let d = rf.s0[j].0 + delta_rps;
                if d > 0 && use_delta[j] {
                    out.s1.push((d, used[j]));
                }
            }
            if delta_rps > 0 && use_delta[n] {
                out.s1.push((delta_rps, used[n]));
            }
            for j in 0..rf.s1.len() {
                let d = rf.s1[j].0 + delta_rps;
                if d > 0 && use_delta[nneg + j] {
                    out.s1.push((d, used[nneg + j]));
                }
            }
            ensure!(out.num_delta_pocs() <= 16, "too many pictures in RPS");
            Ok(out)
        } else {
            let nneg = r.read_ue()? as usize;
            let npos = r.read_ue()? as usize;
            ensure!(nneg <= 16 && npos <= 16 && nneg + npos <= 16, "too many pictures in RPS");
            let mut out = StRps::default();
            let mut poc = 0i32;
            for _ in 0..nneg {
                let d = r.read_ue()? as i32 + 1;
                ensure!(d <= 1 << 15, "delta_poc_s0_minus1 out of range");
                poc -= d;
                out.s0.push((poc, r.read_flag()?));
            }
            poc = 0;
            for _ in 0..npos {
                let d = r.read_ue()? as i32 + 1;
                ensure!(d <= 1 << 15, "delta_poc_s1_minus1 out of range");
                poc += d;
                out.s1.push((poc, r.read_flag()?));
            }
            Ok(out)
        }
    }
}

/// Sequence parameter set.
#[derive(Clone, Debug)]
pub struct Sps {
    pub vps_id: u8,
    pub max_sub_layers_minus1: u32,
    pub ptl: ProfileTierLevel,
    pub id: u32,
    pub chroma_format_idc: u32,
    pub separate_colour_plane: bool,
    pub width: u32,
    pub height: u32,
    /// Conformance window in luma samples (left, right, top, bottom).
    pub conf_win: (u32, u32, u32, u32),
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    pub log2_max_poc_lsb: u32,
    pub max_dec_pic_buffering: u32,
    pub max_num_reorder: u32,
    pub max_latency_increase_plus1: u32,
    pub log2_min_cb: u32,
    pub log2_ctb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub max_th_depth_inter: u32,
    pub max_th_depth_intra: u32,
    pub scaling_list_enabled: bool,
    /// SPS-level scaling list (defaults when enabled without data).
    pub scaling_list: Option<ScalingList>,
    pub amp: bool,
    pub sao: bool,
    pub pcm: bool,
    pub pcm_bit_depth_luma: u32,
    pub pcm_bit_depth_chroma: u32,
    pub log2_min_pcm: u32,
    pub log2_max_pcm: u32,
    pub pcm_loop_filter_disabled: bool,
    pub st_rps: Vec<StRps>,
    pub long_term_refs_present: bool,
    /// lt_ref_pic_poc_lsb_sps, used_by_curr_pic_lt_sps_flag.
    pub lt_ref_pics: Vec<(u32, bool)>,
    pub temporal_mvp: bool,
    pub strong_intra_smoothing: bool,
    pub vui: Option<Vui>,
    /// Range extension flags that change decoding (any set -> unsupported for now).
    pub range_extension: bool,
    pub extension_flags: u8,
}

impl Sps {
    pub fn parse(rbsp: &[u8]) -> Result<Sps> {
        let mut r = BitReader::new(rbsp);
        let vps_id = r.read_bits(4)? as u8;
        let max_sub_layers_minus1 = r.read_bits(3)?;
        ensure!(max_sub_layers_minus1 <= 6, "sps_max_sub_layers_minus1 out of range");
        r.skip(1)?;
        let ptl = parse_ptl(&mut r, true, max_sub_layers_minus1)?;
        let id = r.read_ue()?;
        ensure!(id < 16, "sps_seq_parameter_set_id out of range");
        let chroma_format_idc = r.read_ue()?;
        ensure!(chroma_format_idc <= 3, "chroma_format_idc out of range");
        let separate_colour_plane = if chroma_format_idc == 3 { r.read_flag()? } else { false };
        let width = r.read_ue()?;
        let height = r.read_ue()?;
        check_picture_size(width, height)?;
        let mut conf_win = (0, 0, 0, 0);
        if r.read_flag()? {
            conf_win = (r.read_ue()?, r.read_ue()?, r.read_ue()?, r.read_ue()?);
        }
        let bit_depth_luma = r.read_ue()? + 8;
        let bit_depth_chroma = r.read_ue()? + 8;
        ensure!(bit_depth_luma <= 16 && bit_depth_chroma <= 16, "bit depth out of range");
        let log2_max_poc_lsb = r.read_ue()? + 4;
        ensure!(log2_max_poc_lsb <= 16, "log2_max_pic_order_cnt_lsb_minus4 out of range");
        let ordering_all = r.read_flag()?;
        let (mut dpb, mut reorder, mut latency) = (1, 0, 0);
        let start = if ordering_all { 0 } else { max_sub_layers_minus1 };
        for _ in start..=max_sub_layers_minus1 {
            // the values of the highest sub-layer are used
            dpb = r.read_ue()? + 1;
            reorder = r.read_ue()?;
            latency = r.read_ue()?;
        }
        ensure!(dpb <= 16 && reorder <= 16, "sps_max_dec_pic_buffering out of range");
        let log2_min_cb = r.read_ue()? + 3;
        let log2_ctb = log2_min_cb + r.read_ue()?;
        let log2_min_tb = r.read_ue()? + 2;
        let log2_max_tb = log2_min_tb + r.read_ue()?;
        ensure!((4..=6).contains(&log2_ctb) && log2_min_cb <= log2_ctb, "CTB size out of range");
        ensure!(log2_min_tb < log2_min_cb && log2_max_tb <= log2_ctb.min(5), "transform block sizes out of range");
        ensure!(width % (1 << log2_min_cb) == 0 && height % (1 << log2_min_cb) == 0, "picture size not a multiple of MinCbSizeY");
        let max_th_depth_inter = r.read_ue()?;
        let max_th_depth_intra = r.read_ue()?;
        ensure!(max_th_depth_inter <= log2_ctb - log2_min_tb && max_th_depth_intra <= log2_ctb - log2_min_tb, "transform hierarchy depth out of range");
        let scaling_list_enabled = r.read_flag()?;
        let mut scaling_list = None;
        if scaling_list_enabled {
            scaling_list = Some(if r.read_flag()? { ScalingList::parse(&mut r)? } else { ScalingList::default_lists() });
        }
        let amp = r.read_flag()?;
        let sao = r.read_flag()?;
        let pcm = r.read_flag()?;
        let (mut pcm_bit_depth_luma, mut pcm_bit_depth_chroma, mut log2_min_pcm, mut log2_max_pcm, mut pcm_loop_filter_disabled) = (8, 8, 0, 0, false);
        if pcm {
            pcm_bit_depth_luma = r.read_bits(4)? + 1;
            pcm_bit_depth_chroma = r.read_bits(4)? + 1;
            log2_min_pcm = r.read_ue()? + 3;
            log2_max_pcm = log2_min_pcm + r.read_ue()?;
            pcm_loop_filter_disabled = r.read_flag()?;
            ensure!(pcm_bit_depth_luma <= bit_depth_luma && pcm_bit_depth_chroma <= bit_depth_chroma, "PCM bit depth out of range");
            ensure!(log2_max_pcm <= log2_ctb.min(5), "PCM size out of range");
        }
        let num_st = r.read_ue()? as usize;
        ensure!(num_st <= 64, "num_short_term_ref_pic_sets out of range");
        let mut st_rps = Vec::with_capacity(num_st);
        for i in 0..num_st {
            let s = StRps::parse(&mut r, i, &st_rps, num_st)?;
            st_rps.push(s);
        }
        let long_term_refs_present = r.read_flag()?;
        let mut lt_ref_pics = Vec::new();
        if long_term_refs_present {
            let n = r.read_ue()?;
            ensure!(n <= 32, "num_long_term_ref_pics_sps out of range");
            for _ in 0..n {
                lt_ref_pics.push((r.read_bits(log2_max_poc_lsb)?, r.read_flag()?));
            }
        }
        let temporal_mvp = r.read_flag()?;
        let strong_intra_smoothing = r.read_flag()?;
        let vui = if r.read_flag()? { Some(parse_vui(&mut r, max_sub_layers_minus1)?) } else { None };
        let mut range_extension = false;
        let mut extension_flags = 0;
        if r.read_flag()? {
            extension_flags = r.read_bits(8)? as u8;
            if extension_flags & 0x80 != 0 {
                // sps_range_extension(): any enabled tool is outside Main / Main 10
                let bits = r.read_bits(9)?;
                range_extension = bits != 0;
            }
        }
        Ok(Sps {
            vps_id,
            max_sub_layers_minus1,
            ptl,
            id,
            chroma_format_idc,
            separate_colour_plane,
            width,
            height,
            conf_win,
            bit_depth_luma,
            bit_depth_chroma,
            log2_max_poc_lsb,
            max_dec_pic_buffering: dpb,
            max_num_reorder: reorder,
            max_latency_increase_plus1: latency,
            log2_min_cb,
            log2_ctb,
            log2_min_tb,
            log2_max_tb,
            max_th_depth_inter,
            max_th_depth_intra,
            scaling_list_enabled,
            scaling_list,
            amp,
            sao,
            pcm,
            pcm_bit_depth_luma,
            pcm_bit_depth_chroma,
            log2_min_pcm,
            log2_max_pcm,
            pcm_loop_filter_disabled,
            st_rps,
            long_term_refs_present,
            lt_ref_pics,
            temporal_mvp,
            strong_intra_smoothing,
            vui,
            range_extension,
            extension_flags,
        })
    }

    /// ChromaArrayType.
    pub fn chroma_array_type(&self) -> u32 {
        if self.separate_colour_plane { 0 } else { self.chroma_format_idc }
    }
    pub fn sub_width_c(&self) -> u32 {
        if self.chroma_format_idc == 1 || self.chroma_format_idc == 2 { 2 } else { 1 }
    }
    pub fn sub_height_c(&self) -> u32 {
        if self.chroma_format_idc == 1 { 2 } else { 1 }
    }
    pub fn ctb_size(&self) -> u32 {
        1 << self.log2_ctb
    }
    pub fn pic_width_in_ctbs(&self) -> u32 {
        self.width.div_ceil(self.ctb_size())
    }
    pub fn pic_height_in_ctbs(&self) -> u32 {
        self.height.div_ceil(self.ctb_size())
    }
    pub fn max_poc_lsb(&self) -> i32 {
        1 << self.log2_max_poc_lsb
    }

    /// Cropped output rectangle (x, y, width, height) in luma samples.
    pub fn crop_rect(&self) -> (u32, u32, u32, u32) {
        let (l, r, t, b) = self.conf_win;
        let (sw, sh) = (self.sub_width_c(), self.sub_height_c());
        let x = (l * sw).min(self.width - 1);
        let y = (t * sh).min(self.height - 1);
        let w = self.width.saturating_sub(x + r * sw).max(1);
        let h = self.height.saturating_sub(y + b * sh).max(1);
        (x, y, w, h)
    }

    /// Features this decoder does not handle yet.
    pub fn check_supported(&self) -> Result<()> {
        if self.chroma_format_idc != 1 {
            return unsupported(format!("chroma_format_idc {} (only 4:2:0)", self.chroma_format_idc));
        }
        if self.bit_depth_luma > 12 || self.bit_depth_chroma > 12 {
            return unsupported(format!("bit depth {}/{}", self.bit_depth_luma, self.bit_depth_chroma));
        }
        if self.range_extension {
            return unsupported("SPS range extension tools");
        }
        if self.extension_flags & 0x7f != 0 {
            return unsupported("SPS multilayer / 3D / SCC extensions");
        }
        Ok(())
    }
}

/// Picture parameter set.
#[derive(Clone, Debug)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub dependent_slice_segments_enabled: bool,
    pub output_flag_present: bool,
    pub num_extra_slice_header_bits: u32,
    pub sign_data_hiding: bool,
    pub cabac_init_present: bool,
    pub num_ref_idx_l0_default: u32,
    pub num_ref_idx_l1_default: u32,
    pub init_qp: i32,
    pub constrained_intra_pred: bool,
    pub transform_skip: bool,
    pub cu_qp_delta_enabled: bool,
    pub diff_cu_qp_delta_depth: u32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub slice_chroma_qp_offsets_present: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub transquant_bypass: bool,
    pub tiles_enabled: bool,
    pub entropy_coding_sync: bool,
    pub num_tile_columns: u32,
    pub num_tile_rows: u32,
    pub uniform_spacing: bool,
    /// column_width_minus1 + 1 (explicit spacing only).
    pub column_widths: Vec<u32>,
    pub row_heights: Vec<u32>,
    pub loop_filter_across_tiles: bool,
    pub loop_filter_across_slices: bool,
    pub deblocking_control_present: bool,
    pub deblocking_override_enabled: bool,
    pub deblocking_disabled: bool,
    pub beta_offset_div2: i32,
    pub tc_offset_div2: i32,
    pub scaling_list: Option<ScalingList>,
    pub lists_modification_present: bool,
    pub log2_parallel_merge_level: u32,
    pub slice_header_extension_present: bool,
    pub range_extension: bool,
    pub extension_flags: u8,
}

impl Pps {
    pub fn parse(rbsp: &[u8]) -> Result<Pps> {
        let mut r = BitReader::new(rbsp);
        let id = r.read_ue()?;
        ensure!(id < 64, "pps_pic_parameter_set_id out of range");
        let sps_id = r.read_ue()?;
        ensure!(sps_id < 16, "pps_seq_parameter_set_id out of range");
        let dependent_slice_segments_enabled = r.read_flag()?;
        let output_flag_present = r.read_flag()?;
        let num_extra_slice_header_bits = r.read_bits(3)?;
        let sign_data_hiding = r.read_flag()?;
        let cabac_init_present = r.read_flag()?;
        let num_ref_idx_l0_default = r.read_ue()? + 1;
        let num_ref_idx_l1_default = r.read_ue()? + 1;
        ensure!(num_ref_idx_l0_default <= 15 && num_ref_idx_l1_default <= 15, "num_ref_idx_default_active out of range");
        let init_qp = 26 + r.read_se()?;
        let constrained_intra_pred = r.read_flag()?;
        let transform_skip = r.read_flag()?;
        let cu_qp_delta_enabled = r.read_flag()?;
        let diff_cu_qp_delta_depth = if cu_qp_delta_enabled { r.read_ue()? } else { 0 };
        ensure!(diff_cu_qp_delta_depth <= 3, "diff_cu_qp_delta_depth out of range");
        let cb_qp_offset = r.read_se()?;
        let cr_qp_offset = r.read_se()?;
        ensure!((-12..=12).contains(&cb_qp_offset) && (-12..=12).contains(&cr_qp_offset), "chroma QP offset out of range");
        let slice_chroma_qp_offsets_present = r.read_flag()?;
        let weighted_pred = r.read_flag()?;
        let weighted_bipred = r.read_flag()?;
        let transquant_bypass = r.read_flag()?;
        let tiles_enabled = r.read_flag()?;
        let entropy_coding_sync = r.read_flag()?;
        let (mut num_tile_columns, mut num_tile_rows, mut uniform_spacing) = (1, 1, true);
        let (mut column_widths, mut row_heights) = (Vec::new(), Vec::new());
        let mut loop_filter_across_tiles = true;
        if tiles_enabled {
            num_tile_columns = r.read_ue()? + 1;
            num_tile_rows = r.read_ue()? + 1;
            ensure!(num_tile_columns <= 64 && num_tile_rows <= 64, "too many tiles");
            uniform_spacing = r.read_flag()?;
            if !uniform_spacing {
                for _ in 0..num_tile_columns - 1 {
                    column_widths.push(r.read_ue()?.saturating_add(1));
                }
                for _ in 0..num_tile_rows - 1 {
                    row_heights.push(r.read_ue()?.saturating_add(1));
                }
            }
            loop_filter_across_tiles = r.read_flag()?;
        }
        let loop_filter_across_slices = r.read_flag()?;
        let deblocking_control_present = r.read_flag()?;
        let (mut deblocking_override_enabled, mut deblocking_disabled, mut beta_offset_div2, mut tc_offset_div2) = (false, false, 0, 0);
        if deblocking_control_present {
            deblocking_override_enabled = r.read_flag()?;
            deblocking_disabled = r.read_flag()?;
            if !deblocking_disabled {
                beta_offset_div2 = r.read_se()?;
                tc_offset_div2 = r.read_se()?;
                ensure!((-6..=6).contains(&beta_offset_div2) && (-6..=6).contains(&tc_offset_div2), "deblocking offsets out of range");
            }
        }
        let scaling_list = if r.read_flag()? { Some(ScalingList::parse(&mut r)?) } else { None };
        let lists_modification_present = r.read_flag()?;
        let log2_parallel_merge_level = r.read_ue()? + 2;
        ensure!(log2_parallel_merge_level <= 6, "log2_parallel_merge_level out of range");
        let slice_header_extension_present = r.read_flag()?;
        let mut range_extension = false;
        let mut extension_flags = 0;
        if r.read_flag()? {
            extension_flags = r.read_bits(8)? as u8;
            if extension_flags & 0x80 != 0 {
                // pps_range_extension(): treated as unsupported when it enables anything
                let mut any = false;
                if transform_skip {
                    any |= r.read_ue()? != 0;
                }
                any |= r.read_flag()?; // cross_component_prediction
                any |= r.read_flag()?; // chroma_qp_offset_list
                range_extension = any;
            }
        }
        Ok(Pps {
            id,
            sps_id,
            dependent_slice_segments_enabled,
            output_flag_present,
            num_extra_slice_header_bits,
            sign_data_hiding,
            cabac_init_present,
            num_ref_idx_l0_default,
            num_ref_idx_l1_default,
            init_qp,
            constrained_intra_pred,
            transform_skip,
            cu_qp_delta_enabled,
            diff_cu_qp_delta_depth,
            cb_qp_offset,
            cr_qp_offset,
            slice_chroma_qp_offsets_present,
            weighted_pred,
            weighted_bipred,
            transquant_bypass,
            tiles_enabled,
            entropy_coding_sync,
            num_tile_columns,
            num_tile_rows,
            uniform_spacing,
            column_widths,
            row_heights,
            loop_filter_across_tiles,
            loop_filter_across_slices,
            deblocking_control_present,
            deblocking_override_enabled,
            deblocking_disabled,
            beta_offset_div2,
            tc_offset_div2,
            scaling_list,
            lists_modification_present,
            log2_parallel_merge_level,
            slice_header_extension_present,
            range_extension,
            extension_flags,
        })
    }

    pub fn check_supported(&self) -> Result<()> {
        if self.range_extension {
            return unsupported("PPS range extension tools");
        }
        if self.extension_flags & 0x7f != 0 {
            return unsupported("PPS multilayer / 3D / SCC extensions");
        }
        Ok(())
    }
}

/// CTB addressing for a PPS/SPS pair: tile boundaries and the raster <-> tile scan conversion
/// (6.5.1).
#[derive(Clone, Debug)]
pub struct Layout {
    pub width_ctbs: u32,
    pub height_ctbs: u32,
    /// Tile column boundaries in CTBs (num_tile_columns + 1 entries).
    pub col_bd: Vec<u32>,
    pub row_bd: Vec<u32>,
    pub rs_to_ts: Vec<u32>,
    pub ts_to_rs: Vec<u32>,
    pub tile_id: Vec<u32>,
}

impl Layout {
    pub fn new(sps: &Sps, pps: &Pps) -> Result<Layout> {
        let (w, h) = (sps.pic_width_in_ctbs(), sps.pic_height_in_ctbs());
        let (nc, nr) = (pps.num_tile_columns, pps.num_tile_rows);
        ensure!(nc <= w && nr <= h, "more tiles than CTBs");
        let mut colw = Vec::new();
        let mut rowh = Vec::new();
        if pps.uniform_spacing {
            for i in 0..nc {
                colw.push((i + 1) * w / nc - i * w / nc);
            }
            for j in 0..nr {
                rowh.push((j + 1) * h / nr - j * h / nr);
            }
        } else {
            let sw: u64 = pps.column_widths.iter().map(|&v| v as u64).sum();
            let sh: u64 = pps.row_heights.iter().map(|&v| v as u64).sum();
            ensure!(sw < w as u64 && sh < h as u64, "tile sizes exceed the picture");
            colw.extend_from_slice(&pps.column_widths);
            colw.push(w - sw as u32);
            rowh.extend_from_slice(&pps.row_heights);
            rowh.push(h - sh as u32);
        }
        let mut col_bd = vec![0];
        let mut acc = 0;
        for c in &colw {
            acc += c;
            col_bd.push(acc);
        }
        let mut row_bd = vec![0];
        let mut acc = 0;
        for r in &rowh {
            acc += r;
            row_bd.push(acc);
        }
        let n = (w * h) as usize;
        let mut rs_to_ts = vec![0u32; n];
        for rs in 0..w * h {
            let (tbx, tby) = (rs % w, rs / w);
            let tile_x = (0..nc as usize).rev().find(|&i| tbx >= col_bd[i]).unwrap_or(0);
            let tile_y = (0..nr as usize).rev().find(|&j| tby >= row_bd[j]).unwrap_or(0);
            let mut v = 0;
            for i in 0..tile_x {
                v += rowh[tile_y] * colw[i];
            }
            for j in 0..tile_y {
                v += w * rowh[j];
            }
            v += (tby - row_bd[tile_y]) * colw[tile_x] + tbx - col_bd[tile_x];
            rs_to_ts[rs as usize] = v;
        }
        let mut ts_to_rs = vec![0u32; n];
        for rs in 0..n {
            ts_to_rs[rs_to_ts[rs] as usize] = rs as u32;
        }
        let mut tile_id = vec![0u32; n];
        let mut tid = 0;
        for j in 0..nr as usize {
            for i in 0..nc as usize {
                for y in row_bd[j]..row_bd[j + 1] {
                    for x in col_bd[i]..col_bd[i + 1] {
                        tile_id[rs_to_ts[(y * w + x) as usize] as usize] = tid;
                    }
                }
                tid += 1;
            }
        }
        Ok(Layout { width_ctbs: w, height_ctbs: h, col_bd, row_bd, rs_to_ts, ts_to_rs, tile_id })
    }

    /// Tile id of the CTB at raster address `rs`.
    #[inline]
    pub fn tile_of_rs(&self, rs: u32) -> u32 {
        self.tile_id[self.rs_to_ts[rs as usize] as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_size_has_an_aggregate_pixel_budget() {
        check_picture_size(8192, 8192).unwrap();
        let err = check_picture_size(8192, 8193).unwrap_err();
        assert!(err.to_string().contains("decoded-frame budget"));
    }
}

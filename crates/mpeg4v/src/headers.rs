//! Visual object sequence, visual object, video object layer, group of VOP and VOP headers
//! (ISO/IEC 14496-2 §6.2).

use crate::bits::Bits;
use crate::{Error, Result};

pub(crate) const VOL_MIN: u8 = 0x20;
pub(crate) const VOL_MAX: u8 = 0x2F;
pub(crate) const VOS_START: u8 = 0xB0;
pub(crate) const GOV_START: u8 = 0xB3;
pub(crate) const VO_START: u8 = 0xB5;
pub(crate) const VOP_START: u8 = 0xB6;

/// VOP prediction type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VopType {
    I,
    P,
    B,
    /// Sprite / global motion compensation VOP.
    S,
}

impl VopType {
    fn from_bits(v: u32) -> VopType {
        match v & 3 {
            0 => VopType::I,
            1 => VopType::P,
            2 => VopType::B,
            _ => VopType::S,
        }
    }
}

/// Default quantiser matrices (§6.3.3), raster order.
pub(crate) const DEFAULT_INTRA: [u8; 64] = [
    8, 17, 18, 19, 21, 23, 25, 27, 17, 18, 19, 21, 23, 25, 27, 28, 20, 21, 22, 23, 24, 26, 28, 30, 21, 22, 23, 24, 26, 28, 30, 32, 22, 23, 24, 26, 28, 30, 32,
    35, 23, 24, 26, 28, 30, 32, 35, 38, 25, 26, 28, 30, 32, 35, 38, 41, 27, 28, 30, 32, 35, 38, 41, 45,
];
pub(crate) const DEFAULT_NONINTRA: [u8; 64] = [
    16, 17, 18, 19, 20, 21, 22, 23, 17, 18, 19, 20, 21, 22, 23, 24, 18, 19, 20, 21, 22, 23, 24, 25, 19, 20, 21, 22, 23, 24, 26, 27, 20, 21, 22, 23, 25, 26, 27,
    28, 21, 22, 23, 24, 26, 27, 28, 30, 22, 23, 24, 26, 27, 28, 30, 31, 23, 24, 25, 27, 28, 30, 31, 33,
];

/// Zigzag scan (§7.3.2): coefficient number to raster position.
pub(crate) const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29,
    22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
pub(crate) const ALTERNATE_HORIZONTAL: [u8; 64] = [
    0, 1, 2, 3, 8, 9, 16, 17, 10, 11, 4, 5, 6, 7, 15, 14, 13, 12, 19, 18, 24, 25, 32, 33, 26, 27, 20, 21, 22, 23, 28, 29, 30, 31, 34, 35, 40, 41, 48, 49, 42,
    43, 36, 37, 38, 39, 44, 45, 46, 47, 50, 51, 56, 57, 58, 59, 52, 53, 54, 55, 60, 61, 62, 63,
];
pub(crate) const ALTERNATE_VERTICAL: [u8; 64] = [
    0, 8, 16, 24, 1, 9, 2, 10, 17, 25, 32, 40, 48, 56, 57, 49, 41, 33, 26, 18, 3, 11, 4, 12, 19, 27, 34, 42, 50, 58, 35, 43, 51, 59, 20, 28, 5, 13, 6, 14, 21,
    29, 36, 44, 52, 60, 37, 45, 53, 61, 22, 30, 7, 15, 23, 31, 38, 46, 54, 62, 39, 47, 55, 63,
];

/// What the visual object sequence and visual object headers said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ObjectHeader {
    pub profile_and_level: Option<u8>,
    pub verid: Option<u8>,
    /// (colour_primaries, transfer_characteristics, matrix_coefficients).
    pub colour: Option<(u8, u8, u8)>,
    pub full_range: bool,
}

pub(crate) fn parse_vos(data: &[u8], o: &mut ObjectHeader) {
    if let Some(&p) = data.first() {
        o.profile_and_level = Some(p);
    }
}

pub(crate) fn parse_vo(data: &[u8], o: &mut ObjectHeader) {
    let mut br = Bits::new(data);
    if br.bit() {
        o.verid = Some(br.read(4) as u8);
        br.skip(3); // priority
    }
    let kind = br.read(4);
    if kind == 1 && br.bit() {
        br.skip(3); // video_format
        o.full_range = br.bit();
        if br.bit() {
            o.colour = Some((br.read(8) as u8, br.read(8) as u8, br.read(8) as u8));
        }
    }
}

/// A video object layer. Features the decoder lacks are named in `unsupported`: the size and
/// aspect are still known.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Vol {
    pub object_type: u8,
    pub verid: u8,
    pub width: u32,
    pub height: u32,
    pub sar: (u32, u32),
    pub low_delay: Option<bool>,
    pub time_resolution: u32,
    pub time_bits: u32,
    pub fixed_increment: Option<u32>,
    pub interlaced: bool,
    pub sprite_enable: u8,
    pub mpeg_quant: bool,
    pub intra_matrix: [u8; 64],
    pub nonintra_matrix: [u8; 64],
    pub quarter_sample: bool,
    pub resync_disable: bool,
    pub data_partitioned: bool,
    pub reversible_vlc: bool,
    pub unsupported: Option<&'static str>,
}

/// Table 6-12 / pixel aspect ratios (width, height).
const PAR: [(u32, u32); 6] = [(0, 0), (1, 1), (12, 11), (10, 11), (16, 11), (40, 33)];

fn read_matrix(br: &mut Bits, out: &mut [u8; 64]) {
    let mut last = 0u8;
    let mut n = 0usize;
    for &pos in &ZIGZAG {
        let v = br.read(8) as u8;
        if v == 0 {
            break;
        }
        last = v;
        n += 1;
        if let Some(o) = out.get_mut(pos as usize) {
            *o = v;
        }
    }
    for &pos in ZIGZAG.iter().skip(n) {
        if let Some(o) = out.get_mut(pos as usize) {
            *o = last;
        }
    }
}

pub(crate) fn parse_vol(data: &[u8], object: &ObjectHeader) -> Result<Vol> {
    let mut br = Bits::new(data);
    br.skip(1); // random_accessible_vol
    let object_type = br.read(8) as u8;
    let mut verid = object.verid.unwrap_or(1);
    if br.bit() {
        verid = br.read(4) as u8;
        br.skip(3); // priority
    }
    let mut sar = (1, 1);
    let aspect = br.read(4);
    if aspect == 15 {
        sar = (br.read(8), br.read(8));
    } else if let Some(&p) = PAR.get(aspect as usize)
        && p.0 > 0
    {
        sar = p;
    }
    let mut low_delay = None;
    if br.bit() {
        let chroma = br.read(2);
        if chroma != 1 {
            return Err(Error::Unsupported(format!("chroma_format {chroma} (only 4:2:0 exists in the visual profiles)")));
        }
        low_delay = Some(br.bit());
        if br.bit() {
            br.skip(15 + 1 + 15 + 1 + 15 + 1 + 3 + 11 + 1 + 15 + 1); // vbv parameters
        }
    }
    let shape = br.read(2);
    if shape == 3 && verid != 1 {
        br.skip(4);
    }
    br.skip(1); // marker
    let time_resolution = br.read(16);
    if time_resolution == 0 {
        return Err(Error::Invalid("vop_time_increment_resolution is 0".into()));
    }
    br.skip(1); // marker
    let time_bits = (32 - (time_resolution - 1).leading_zeros()).max(1);
    let fixed_increment = br.bit().then(|| br.read(time_bits));
    let mut vol = Vol {
        object_type,
        verid,
        width: 0,
        height: 0,
        sar,
        low_delay,
        time_resolution,
        time_bits,
        fixed_increment,
        interlaced: false,
        sprite_enable: 0,
        mpeg_quant: false,
        intra_matrix: DEFAULT_INTRA,
        nonintra_matrix: DEFAULT_NONINTRA,
        quarter_sample: false,
        resync_disable: false,
        data_partitioned: false,
        reversible_vlc: false,
        unsupported: None,
    };
    if shape != 0 {
        vol.unsupported = Some("arbitrary shape (non-rectangular video objects)");
        return Ok(vol);
    }
    br.skip(1);
    vol.width = br.read(13);
    br.skip(1);
    vol.height = br.read(13);
    br.skip(1);
    if vol.width == 0 || vol.height == 0 {
        return Err(Error::Invalid(format!("{}x{} video object layer", vol.width, vol.height)));
    }
    vol.interlaced = br.bit();
    let obmc_disable = br.bit();
    vol.sprite_enable = if verid == 1 { br.read(1) as u8 } else { br.read(2) as u8 };
    if vol.sprite_enable == 1 || vol.sprite_enable == 2 {
        if vol.sprite_enable == 1 {
            br.skip(13 + 1 + 13 + 1 + 13 + 1 + 13 + 1);
        }
        br.skip(6 + 2 + 1);
        if vol.sprite_enable == 1 {
            br.skip(1);
        }
    }
    if br.bit() {
        vol.unsupported = Some("more than 8 bits per sample");
        return Ok(vol);
    }
    vol.mpeg_quant = br.bit();
    if vol.mpeg_quant {
        if br.bit() {
            read_matrix(&mut br, &mut vol.intra_matrix);
        }
        if br.bit() {
            read_matrix(&mut br, &mut vol.nonintra_matrix);
        }
    }
    if verid != 1 {
        vol.quarter_sample = br.bit();
    }
    if !br.bit() {
        vol.unsupported = Some("VOP complexity estimation headers");
        return Ok(vol);
    }
    vol.resync_disable = br.bit();
    vol.data_partitioned = br.bit();
    if vol.data_partitioned {
        vol.reversible_vlc = br.bit();
    }
    let mut reduced = false;
    if verid != 1 {
        if br.bit() {
            vol.unsupported = Some("NEWPRED");
            return Ok(vol);
        }
        reduced = br.bit();
    }
    let scalable = br.bit();
    vol.unsupported = if br.overrun() {
        return Err(Error::Invalid("truncated video object layer header".into()));
    } else if vol.interlaced {
        Some("interlaced coding")
    } else if !obmc_disable {
        Some("overlapped block motion compensation")
    } else if vol.sprite_enable == 1 {
        Some("static sprites")
    } else if vol.reversible_vlc {
        Some("reversible VLCs")
    } else if reduced {
        Some("reduced resolution VOPs")
    } else if scalable {
        Some("scalability")
    } else {
        None
    };
    Ok(vol)
}

/// Group of VOP header: the time code in seconds.
pub(crate) fn parse_gov(data: &[u8]) -> u32 {
    let mut br = Bits::new(data);
    let h = br.read(5);
    let m = br.read(6);
    br.skip(1);
    let s = br.read(6);
    h * 3600 + m * 60 + s
}

/// The fields of a VOP header; `data_pos` is the bit position of the macroblock data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VopHeader {
    pub kind: VopType,
    /// modulo_time_base: whole seconds since the time base.
    pub modulo: u32,
    pub increment: u32,
    pub coded: bool,
    pub rounding: u8,
    pub dc_thr: u8,
    pub quant: u8,
    pub fcode_f: u8,
    pub fcode_b: u8,
    pub data_pos: usize,
}

fn read_modulo(br: &mut Bits) -> Option<u32> {
    let mut m = 0;
    while br.bit() {
        m += 1;
        if br.overrun() || m > 1 << 20 {
            return None;
        }
    }
    Some(m)
}

pub(crate) fn parse_vop(data: &[u8], vol: &Vol) -> Result<VopHeader> {
    let mut br = Bits::new(data);
    let kind = VopType::from_bits(br.read(2));
    let modulo = read_modulo(&mut br).ok_or_else(|| Error::Invalid("VOP time base".into()))?;
    br.skip(1);
    let increment = br.read(vol.time_bits);
    br.skip(1);
    let coded = br.bit();
    let mut h = VopHeader { kind, modulo, increment, coded, rounding: 0, dc_thr: 0, quant: 0, fcode_f: 1, fcode_b: 1, data_pos: br.pos() };
    if !coded {
        return Ok(h);
    }
    if kind == VopType::S {
        return Err(match vol.sprite_enable {
            2 => Error::Unsupported("global motion compensation (GMC)".into()),
            1 => Error::Unsupported("static sprites".into()),
            _ => Error::Invalid("S-VOP in a layer without sprites".into()),
        });
    }
    if kind == VopType::P {
        h.rounding = br.read(1) as u8;
    }
    h.dc_thr = br.read(3) as u8;
    h.quant = br.read(5) as u8;
    if h.quant == 0 {
        return Err(Error::Invalid("vop_quant is 0".into()));
    }
    if kind != VopType::I {
        h.fcode_f = br.read(3) as u8;
        if h.fcode_f == 0 {
            return Err(Error::Invalid("vop_fcode_forward is 0".into()));
        }
    }
    if kind == VopType::B {
        h.fcode_b = br.read(3) as u8;
        if h.fcode_b == 0 {
            return Err(Error::Invalid("vop_fcode_backward is 0".into()));
        }
    }
    if br.overrun() {
        return Err(Error::Invalid("truncated VOP header".into()));
    }
    h.data_pos = br.pos();
    Ok(h)
}

/// The packet header fields after a resync marker the decoder keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PacketHeader {
    pub mb: usize,
    pub quant: u8,
}

/// A video packet header (§6.2.5.2); `br` stands just after the resync marker.
pub(crate) fn parse_packet(br: &mut Bits, mb_bits: u32, vol: &Vol, hdr: &VopHeader) -> Option<PacketHeader> {
    let mb = br.read(mb_bits) as usize;
    let quant = br.read(5) as u8;
    if quant == 0 {
        return None;
    }
    if br.bit() {
        read_modulo(br)?;
        br.skip(1);
        br.skip(vol.time_bits);
        br.skip(1);
        let kind = VopType::from_bits(br.read(2));
        br.skip(3); // intra_dc_vlc_thr
        if kind != VopType::I {
            br.skip(3);
        }
        if kind == VopType::B {
            br.skip(3);
        }
        if kind != hdr.kind {
            return None;
        }
    }
    Some(PacketHeader { mb, quant })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_visit_every_coefficient_once() {
        for s in [&ZIGZAG, &ALTERNATE_HORIZONTAL, &ALTERNATE_VERTICAL] {
            let mut seen = [false; 64];
            for &p in s.iter() {
                assert!(!seen[p as usize]);
                seen[p as usize] = true;
            }
        }
    }

    #[test]
    fn a_short_matrix_repeats_its_last_value() {
        let mut d = [0u8; 8];
        d[0] = 9;
        d[1] = 5;
        let mut m = [0u8; 64];
        read_matrix(&mut Bits::new(&d), &mut m);
        assert_eq!((m[0], m[1], m[8], m[63]), (9, 5, 5, 5));
    }
}

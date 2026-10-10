//! What is specific to HEVC in the NVENC backend (safe code): the level codes, and the `hvcC`
//! record built from the parameter sets the encoder wrote.
//!
//! Main (8-bit) and Main 10 (10-bit) are both written; the caller says which it asked the encoder for
//! and the record is refused if the SPS says otherwise.
//!
//! Nothing in the record is invented: the profile, tier, compatibility flags, level, chroma format,
//! bit depths and temporal layers come from the SPS (parsed with `filmcraft_hevc`), and the 48
//! general constraint flags and `sps_temporal_id_nesting_flag` are read from the SPS bits that carry
//! them (`parse_ptl` skips the flags and `Sps` does not keep the nesting flag). The array
//! completeness flags are 1 and the parameter sets are only in the record, never in the samples
//! (the `hvc1` sample entry).

use filmcraft_bitstream::unescape_rbsp;
use filmcraft_hevc::params::Sps;
use filmcraft_isobmff::{HevcConfig, HevcNalArray};

use super::{Codec, nal_type};

/// `NV_ENC_LEVEL_HEVC_*` for a level × 10 (4.1 is 41): HEVC levels are numbered `level × 30`.
/// `None` for a number that is not an HEVC level.
pub fn level_code(level_x10: u8) -> Option<u8> {
    matches!(level_x10, 10 | 20 | 21 | 30 | 31 | 40 | 41 | 50 | 51 | 52 | 60 | 61 | 62).then(|| level_x10.saturating_mul(3))
}

/// The lowest HEVC level (× 10) whose **Main tier** limits take `width × height` pictures at `fps`
/// and a peak bitrate (and, with the one-second VBV buffer, a CPB) of `max_kbps` (H.265 Table A.8,
/// Main / Main 10 profiles: `MaxLumaPs`, `MaxLumaSr`, `MaxBR`, and pictures no wider or taller than
/// `sqrt(8 × MaxLumaPs)`). `None` when no Main tier level does (the encoder then chooses).
pub fn main_tier_level(width: u32, height: u32, fps: (u32, u32), max_kbps: u32) -> Option<u8> {
    // (level × 10, MaxLumaPs, MaxLumaSr, Main tier MaxBR = MaxCPB in kbit/s)
    const LEVELS: [(u8, u64, u64, u64); 13] = [
        (10, 36_864, 552_960, 128),
        (20, 122_880, 3_686_400, 1_500),
        (21, 245_760, 7_372_800, 3_000),
        (30, 552_960, 16_588_800, 6_000),
        (31, 983_040, 33_177_600, 10_000),
        (40, 2_228_224, 66_846_720, 12_000),
        (41, 2_228_224, 133_693_440, 20_000),
        (50, 8_912_896, 267_386_880, 25_000),
        (51, 8_912_896, 534_773_760, 40_000),
        (52, 8_912_896, 1_069_547_520, 60_000),
        (60, 35_651_584, 1_069_547_520, 60_000),
        (61, 35_651_584, 2_139_095_040, 120_000),
        (62, 35_651_584, 4_278_190_080, 240_000),
    ];
    if fps.1 == 0 {
        return None;
    }
    let ps = u64::from(width).checked_mul(u64::from(height))?;
    // luma samples per second, rounded up
    let sr = ps.checked_mul(u64::from(fps.0))?.div_ceil(u64::from(fps.1));
    let side = u64::from(width.max(height));
    LEVELS
        .iter()
        .find(|(_, max_ps, max_sr, max_br)| {
            // side² ≤ 8 × MaxLumaPs: the dimension limit without a square root
            ps <= *max_ps && sr <= *max_sr && u64::from(max_kbps) <= *max_br && side.saturating_mul(side) <= max_ps.saturating_mul(8)
        })
        .map(|(l, ..)| *l)
}

/// Bytes of the SPS RBSP before the general profile / tier / level record: `sps_video_parameter_set_id`,
/// `sps_max_sub_layers_minus1` and `sps_temporal_id_nesting_flag` share the first one.
const PTL_START: usize = 1;
/// General profile space / tier / profile (1 byte), compatibility flags (4), constraint flags (6), level (1).
const PTL_LEN: usize = 12;

/// The `hvcC` record of a stream of `size` pictures (width, height) whose parameter sets are `vps`,
/// `sps` and `pps` (NAL units with their headers, without start codes or length prefixes). An error
/// says why the stream is not the one that was asked for: `bit_depth` 8 is HEVC Main (profile 1),
/// 10 is Main 10 (profile 2), both 4:2:0 with equal luma and chroma depth; any other depth is refused.
pub fn hevc_config(vps: &[u8], sps: &[u8], pps: &[u8], size: (u32, u32), bit_depth: u8) -> Result<HevcConfig, String> {
    let (profile_idc, profile_name) = match bit_depth {
        8 => (1u8, "Main"),
        10 => (2, "Main 10"),
        other => return Err(format!("{other}-bit HEVC is not written by this backend")),
    };
    for (nal, kind, name) in [(vps, 32u8, "VPS"), (sps, 33, "SPS"), (pps, 34, "PPS")] {
        if nal_type(Codec::Hevc, nal) != Some(kind) || nal.len() <= 2 {
            return Err(format!("the encoder's {name} is not a {name} NAL unit"));
        }
        // the record stores each length in 16 bits
        if u16::try_from(nal.len()).is_err() {
            return Err(format!("the {name} is too long for an hvcC record"));
        }
    }
    let rbsp = unescape_rbsp(sps.get(2..).unwrap_or_default());
    let parsed = Sps::parse(&rbsp).map_err(|e| format!("unreadable HEVC SPS: {e}"))?;
    let ptl_bytes = rbsp.get(PTL_START..PTL_START + PTL_LEN).ok_or("the HEVC SPS is truncated before its profile / tier / level")?;
    // bytes 5..11 of the record are the 48 general constraint indicator flags
    let constraints = ptl_bytes.get(5..11).ok_or("the HEVC SPS is truncated before its constraint flags")?;
    let general_constraint_indicator_flags = constraints.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    let temporal_id_nested = rbsp.first().is_some_and(|b| b & 1 != 0);

    let ptl = &parsed.ptl;
    if ptl.profile_space != 0 || ptl.profile_idc != profile_idc {
        return Err(format!("the encoder wrote HEVC profile {} (space {}), not {profile_name}", ptl.profile_idc, ptl.profile_space));
    }
    let depth = u32::from(bit_depth);
    if parsed.chroma_format_idc != 1 || parsed.bit_depth_luma != depth || parsed.bit_depth_chroma != depth {
        return Err(format!(
            "the encoder wrote chroma format {} at {}/{} bits, not {bit_depth}-bit 4:2:0",
            parsed.chroma_format_idc, parsed.bit_depth_luma, parsed.bit_depth_chroma
        ));
    }
    let (_, _, w, h) = parsed.crop_rect();
    if (w, h) != size {
        return Err(format!("the encoder wrote {w}x{h} pictures for a {}x{} export", size.0, size.1));
    }
    Ok(HevcConfig {
        general_profile_space: ptl.profile_space,
        general_tier_flag: ptl.tier,
        general_profile_idc: ptl.profile_idc,
        general_profile_compatibility_flags: ptl.compatibility,
        general_constraint_indicator_flags,
        general_level_idc: ptl.level_idc,
        // 0: not specified, which is a valid value for both
        min_spatial_segmentation_idc: 0,
        parallelism_type: 0,
        chroma_format_idc: u8::try_from(parsed.chroma_format_idc).map_err(|_| "chroma format".to_string())?,
        bit_depth_luma: u8::try_from(parsed.bit_depth_luma).map_err(|_| "bit depth".to_string())?,
        bit_depth_chroma: u8::try_from(parsed.bit_depth_chroma).map_err(|_| "bit depth".to_string())?,
        // 0: unspecified average rate, and the frame rate may not be constant
        avg_frame_rate: 0,
        constant_frame_rate: 0,
        num_temporal_layers: u8::try_from(parsed.max_sub_layers_minus1.saturating_add(1)).map_err(|_| "temporal layers".to_string())?,
        temporal_id_nested,
        length_size: 4,
        arrays: vec![
            HevcNalArray { completeness: true, nal_type: 32, nalus: vec![vps.to_vec()] },
            HevcNalArray { completeness: true, nal_type: 33, nalus: vec![sps.to_vec()] },
            HevcNalArray { completeness: true, nal_type: 34, nalus: vec![pps.to_vec()] },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The parameter sets NVENC wrote for a 1280x720, 24 fps, B-frame HEVC Main stream (RTX 5060, driver
    // 617.14): the coded size is 1280x736 and a conformance window crops it to 1280x720.
    const VPS: &str = "40010c01ffff01600000030090000003000003005d9940c0000003004000000614";
    const SPS: &str = "42010101600000030090000003000003005da00280802e1f1396654a421191bff0c05a8080808a0000030002000003003010";
    const PPS: &str = "4401c1937c0cc9";

    fn unhex(h: &str) -> Vec<u8> {
        (0..h.len()).step_by(2).filter_map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect()
    }

    fn record() -> HevcConfig {
        hevc_config(&unhex(VPS), &unhex(SPS), &unhex(PPS), (1280, 720), 8).unwrap()
    }

    #[test]
    fn hevc_levels_are_thirty_times_the_level() {
        assert_eq!(level_code(41), Some(123));
        assert_eq!(level_code(10), Some(30));
        assert_eq!(level_code(21), Some(63));
        assert_eq!(level_code(62), Some(186));
        for bad in [0, 11, 42, 53, 70, 255] {
            assert_eq!(level_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_level_is_the_lowest_main_tier_one_that_fits() {
        // size and sample rate alone
        assert_eq!(main_tier_level(1280, 720, (30, 1), 5_000), Some(31));
        assert_eq!(main_tier_level(1920, 1080, (30, 1), 8_000), Some(40));
        // the bitrate moves 1080p30 up a level instead of to the High tier (what NVENC picked by itself)
        assert_eq!(main_tier_level(1920, 1080, (30, 1), 18_000), Some(41));
        assert_eq!(main_tier_level(1920, 1080, (60, 1), 12_000), Some(41));
        assert_eq!(main_tier_level(3840, 2160, (30, 1), 22_500), Some(50));
        assert_eq!(main_tier_level(3840, 2160, (30, 1), 60_000), Some(52));
        assert_eq!(main_tier_level(3840, 2160, (60, 1), 60_000), Some(52));
        assert_eq!(main_tier_level(7680, 4320, (30, 1), 60_000), Some(60));
        // 29.97 rounds the sample rate up
        assert_eq!(main_tier_level(1920, 1080, (30_000, 1_001), 8_000), Some(40));
        // a long thin picture is held by the dimension limit, not the area
        assert_eq!(main_tier_level(8192, 64, (30, 1), 1_000), Some(50));
        // beyond every Main tier level, or nonsense: the encoder chooses
        assert_eq!(main_tier_level(1920, 1080, (30, 1), 300_000), None);
        assert_eq!(main_tier_level(16_384, 16_384, (30, 1), 1_000), None);
        assert_eq!(main_tier_level(1920, 1080, (30, 0), 1_000), None);
        assert_eq!(main_tier_level(u32::MAX, u32::MAX, (u32::MAX, 1), u32::MAX), None);
        // every answer is a level the encoder takes
        assert!(main_tier_level(1920, 1080, (30, 1), 18_000).and_then(level_code).is_some());
    }

    #[test]
    fn the_record_is_the_sps_it_came_from() {
        let c = record();
        let sps = unhex(SPS);
        let rbsp = unescape_rbsp(&sps[2..]);
        // profile / tier / compatibility / constraint flags / level are the SPS's own bytes
        assert_eq!(&c.to_bytes()[1..13], &rbsp[1..13]);
        assert_eq!((c.general_profile_space, c.general_tier_flag, c.general_profile_idc), (0, false, 1));
        assert_eq!(c.general_profile_compatibility_flags, 0x6000_0000, "Main and Main 10");
        // progressive_source_flag and frame_only_constraint_flag
        assert_eq!(c.general_constraint_indicator_flags, 0x9000_0000_0000);
        assert_eq!(c.general_level_idc, 93, "level 3.1");
        assert_eq!((c.chroma_format_idc, c.bit_depth_luma, c.bit_depth_chroma), (1, 8, 8));
        assert_eq!((c.num_temporal_layers, c.temporal_id_nested, c.length_size), (1, true, 4));
        assert_eq!(c.arrays.iter().map(|a| (a.nal_type, a.completeness, a.nalus.len())).collect::<Vec<_>>(), vec![(32, true, 1), (33, true, 1), (34, true, 1)]);
        assert_eq!((c.vps(), c.sps(), c.pps()), (vec![&unhex(VPS)[..]], vec![&sps[..]], vec![&unhex(PPS)[..]]));
        assert_eq!(HevcConfig::parse(&c.to_bytes()).unwrap(), c);
    }

    #[test]
    fn the_picture_size_has_to_match_after_cropping() {
        let (v, s, p) = (unhex(VPS), unhex(SPS), unhex(PPS));
        for wrong in [(1280, 736), (1920, 1080), (1280, 719), (0, 0)] {
            let e = hevc_config(&v, &s, &p, wrong, 8).unwrap_err();
            assert!(e.contains("1280x720"), "{e}");
        }
    }

    #[test]
    fn a_stream_that_is_not_main_8_bit_420_is_refused() {
        // The same SPS with the general profile changed (bits of profile_idc in the first PTL byte),
        // the chroma format or the bit depth changed would be other streams: edit the PTL byte only
        // (it sits before any emulation-prevention byte).
        let (v, s, p) = (unhex(VPS), unhex(SPS), unhex(PPS));
        let mut other_profile = s.clone();
        other_profile[3] = (other_profile[3] & !0x1f) | 2; // Main 10
        assert!(hevc_config(&v, &other_profile, &p, (1280, 720), 8).unwrap_err().contains("not Main"));
        let mut other_space = s.clone();
        other_space[3] |= 0x40;
        assert!(hevc_config(&v, &other_space, &p, (1280, 720), 8).is_err());
    }

    #[test]
    fn truncated_and_corrupt_parameter_sets_are_errors_never_panics() {
        let (v, s, p) = (unhex(VPS), unhex(SPS), unhex(PPS));
        // every truncation of the SPS
        for n in 0..s.len() {
            assert!(hevc_config(&v, &s[..n], &p, (1280, 720), 8).is_err(), "SPS cut at {n}");
        }
        // a bit flip anywhere in the SPS: an error or a record, but never a panic
        for byte in 0..s.len() {
            for bit in 0..8 {
                let mut m = s.clone();
                m[byte] ^= 1 << bit;
                let r = std::panic::catch_unwind(|| hevc_config(&v, &m, &p, (1280, 720), 8));
                assert!(r.is_ok(), "panic with bit {bit} of byte {byte} flipped");
            }
        }
        // wrong NAL types, empty and one-byte NAL units
        let cases: [(&[u8], &[u8], &[u8]); 5] = [
            (&[], &[], &[]),
            (&[0x40], &[0x42], &[0x44]),
            (&[0x40, 0x01, 0xAA], &[0x42, 0x01], &[0x44, 0x01, 0xBB]),
            (&[0x42, 0x01, 0xAA], &s, &p),
            (&v, &p, &s),
        ];
        for (v, s, p) in cases {
            assert!(hevc_config(v, s, p, (64, 64), 8).is_err(), "{v:?} {s:?} {p:?}");
        }
        // a parameter set too long for the record's 16-bit lengths
        let mut huge = s.clone();
        huge.resize(70_000, 0x55);
        assert!(hevc_config(&v, &huge, &p, (1280, 720), 8).is_err());
    }

    // The parameter sets NVENC wrote for a 1280x720, 24 fps HEVC Main 10 stream with the PQ signal (VUI: BT.2020
    // primaries, SMPTE ST 2084, BT.2020 NCL matrix, limited range) on the same GPU.
    const VPS10: &str = "40010c01ffff2220000003009000000300000300789940c0000003004000000614";
    const SPS10: &str = "420101222000000300900000030000030078a00280802e1f12d96654a421191bff0c05a8488048a000000300200000030301";
    const PPS10: &str = "4401c1933c0cc9";

    #[test]
    fn a_main_10_stream_is_accepted_as_main_10_and_refused_as_main() {
        let (v, s, p) = (unhex(VPS10), unhex(SPS10), unhex(PPS10));
        let c = hevc_config(&v, &s, &p, (1280, 720), 10).unwrap();
        assert_eq!((c.general_profile_space, c.general_tier_flag, c.general_profile_idc), (0, true, 2), "NVENC picked the High tier for level 4 at 12 Mbps");
        assert_eq!(c.general_profile_compatibility_flags, 0x2000_0000, "Main 10 only");
        assert_eq!(c.general_constraint_indicator_flags, 0x9000_0000_0000);
        assert_eq!(c.general_level_idc, 120, "level 4");
        assert_eq!((c.chroma_format_idc, c.bit_depth_luma, c.bit_depth_chroma), (1, 10, 10));
        assert_eq!(HevcConfig::parse(&c.to_bytes()).unwrap(), c);
        // the same stream where Main (8-bit) was asked for, and the 8-bit stream where Main 10 was asked for
        let e = hevc_config(&v, &s, &p, (1280, 720), 8).unwrap_err();
        assert!(e.contains("not Main") && !e.contains("Main 10"), "{e}");
        let e = hevc_config(&unhex(VPS), &unhex(SPS), &unhex(PPS), (1280, 720), 10).unwrap_err();
        assert!(e.contains("not Main 10"), "{e}");
        // no other depth
        for depth in [0, 7, 9, 12, 16, 255] {
            assert!(hevc_config(&v, &s, &p, (1280, 720), depth).is_err(), "{depth}");
        }
        // a Main 10 profile byte with 8-bit depth (or the reverse) is refused whichever is asked for
        let mut forged = s.clone();
        forged[3] = (forged[3] & !0x1f) | 1; // profile_idc 1 (Main) but a 10-bit SPS
        for depth in [8, 10] {
            assert!(hevc_config(&v, &forged, &p, (1280, 720), depth).is_err(), "{depth}");
        }
    }

    #[test]
    fn truncated_and_flipped_main_10_parameter_sets_never_panic() {
        let (v, s, p) = (unhex(VPS10), unhex(SPS10), unhex(PPS10));
        for n in 0..s.len() {
            assert!(hevc_config(&v, &s[..n], &p, (1280, 720), 10).is_err(), "SPS cut at {n}");
        }
        for byte in 0..s.len() {
            for bit in 0..8 {
                let mut m = s.clone();
                m[byte] ^= 1 << bit;
                assert!(std::panic::catch_unwind(|| hevc_config(&v, &m, &p, (1280, 720), 10)).is_ok(), "panic with bit {bit} of byte {byte} flipped");
            }
        }
    }
}

//! What is specific to AV1 in the NVENC backend (safe code): the `av1C` record built from the
//! sequence header OBU the encoder wrote.
//!
//! Main (8-bit) and Main 10 (10-bit) 4:2:0 profiles are supported.

use craft_video::av1::decode::SequenceHeader;
use filmcraft_isobmff::Av1Config;

/// Extract the Sequence Header OBU from low-overhead AV1 OBUs (as NVENC outputs them).
pub fn extract_sequence_header_obu(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut pos = 0usize;
    while let Some(&h) = data.get(pos) {
        if h & 0x81 != 0 {
            return None;
        }
        let (obu_type, ext, has_size) = ((h >> 3) & 0xf, h & 4 != 0, h & 2 != 0);
        let start = pos;
        let mut p = pos + 1 + usize::from(ext);
        let size = if has_size {
            let (mut v, mut shift, mut n) = (0u64, 0u32, 0usize);
            loop {
                let &b = data.get(p + n)?;
                v |= u64::from(b & 0x7f) << shift;
                n += 1;
                if b & 0x80 == 0 {
                    break;
                }
                if n >= 8 {
                    return None;
                }
                shift += 7;
            }
            p += n;
            usize::try_from(v).ok()?
        } else {
            data.len().saturating_sub(p)
        };
        let payload = data.get(p..p.saturating_add(size))?;
        let full_obu = data.get(start..p.saturating_add(size))?;
        if obu_type == 1 {
            return Some((full_obu, payload));
        }
        pos = p.saturating_add(size);
    }
    None
}

/// AV1 spec 5.8 / 6.7: HDR metadata has RGB primaries and binary fixed-point units,
/// unlike the GBR primaries and decimal units used by HEVC SEI / ISOBMFF mdcv.
pub fn hdr_metadata(signal: &super::Signal) -> Result<Vec<u8>, String> {
    if signal.sei.len() > 2 {
        return Err("too many AV1 HDR metadata messages".into());
    }
    let mut out = Vec::new();
    for message in &signal.sei {
        let mut payload = Vec::new();
        match (message.payload_type, message.payload.len()) {
            (144, 4) => {
                payload.push(1);
                payload.extend_from_slice(&message.payload);
            }
            (137, 24) => {
                payload.push(2);
                let values: Vec<u16> = message.payload[..16].as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect();
                for i in [4, 5, 0, 1, 2, 3, 6, 7] {
                    let v = u64::from(values[i]);
                    if v > 50_000 {
                        return Err("invalid mastering display chromaticity".into());
                    }
                    let scaled = ((v * 65_536 + 25_000) / 50_000).min(65_535) as u16;
                    payload.extend_from_slice(&scaled.to_be_bytes());
                }
                for (offset, scale) in [(16, 256u64), (20, 16_384)] {
                    let bytes: [u8; 4] =
                        message.payload.get(offset..offset + 4).ok_or("short mastering metadata")?.try_into().map_err(|_| "short mastering metadata")?;
                    let value = (u64::from(u32::from_be_bytes(bytes)) * scale + 5_000) / 10_000;
                    let value = u32::try_from(value).map_err(|_| "mastering luminance exceeds AV1 range")?;
                    payload.extend_from_slice(&value.to_be_bytes());
                }
            }
            _ => return Err("unsupported or malformed AV1 HDR metadata".into()),
        }
        payload.push(0x80); // trailing_bits()
        out.push(0x2a); // OBU_METADATA, has_size_field
        out.push(u8::try_from(payload.len()).map_err(|_| "AV1 metadata too large")?);
        out.extend(payload);
    }
    Ok(out)
}

/// Insert static metadata after the sequence header / delimiter, before the first frame.
pub fn insert_metadata(packet: &[u8], metadata: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 0usize;
    while let Some(&h) = packet.get(pos) {
        if matches!((h >> 3) & 15, 3 | 6) {
            let mut out = Vec::with_capacity(packet.len().saturating_add(metadata.len()));
            out.extend_from_slice(&packet[..pos]);
            out.extend_from_slice(metadata);
            out.extend_from_slice(&packet[pos..]);
            return Ok(out);
        }
        if h & 2 == 0 {
            return Err("NVENC AV1 OBU has no size".into());
        }
        let mut p = pos + 1 + usize::from(h & 4 != 0);
        let mut len = 0u64;
        let mut complete = false;
        for i in 0..8 {
            let &b = packet.get(p).ok_or("short AV1 OBU size")?;
            p += 1;
            len |= u64::from(b & 127) << (7 * i);
            if b & 128 == 0 {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err("invalid AV1 OBU size".into());
        }
        pos = p.checked_add(usize::try_from(len).map_err(|_| "AV1 OBU size overflow")?).filter(|&end| end <= packet.len()).ok_or("short AV1 OBU")?;
    }
    Err("NVENC AV1 packet has no frame".into())
}

/// Read `seq_tier[0]` from an AV1 sequence header OBU payload (AV1 spec 5.5.1).
fn parse_seq_tier_0(payload: &[u8]) -> Option<bool> {
    let mut bit_pos = 0usize;
    let total_bits = payload.len().checked_mul(8)?;
    let read_bits = |pos: &mut usize, n: usize| -> Option<u32> {
        if n > 32 || pos.checked_add(n)? > total_bits {
            return None;
        }
        let mut val = 0u32;
        for _ in 0..n {
            let byte = *payload.get(*pos >> 3)?;
            let bit = (byte >> (7 - (*pos & 7))) & 1;
            val = (val << 1) | u32::from(bit);
            *pos += 1;
        }
        Some(val)
    };
    let skip_uvlc = |pos: &mut usize| -> Option<()> {
        let mut leading_zeros = 0usize;
        while read_bits(pos, 1)? == 0 {
            leading_zeros += 1;
            if leading_zeros >= 32 {
                return Some(());
            }
        }
        let _ = read_bits(pos, leading_zeros)?;
        Some(())
    };

    let _seq_profile = read_bits(&mut bit_pos, 3)?;
    let _still_picture = read_bits(&mut bit_pos, 1)?;
    let reduced = read_bits(&mut bit_pos, 1)? != 0;
    if reduced {
        return Some(false);
    }
    let timing_info_present = read_bits(&mut bit_pos, 1)? != 0;
    if timing_info_present {
        let _num_units_in_display_tick = read_bits(&mut bit_pos, 32)?;
        let _time_scale = read_bits(&mut bit_pos, 32)?;
        let equal_picture_interval = read_bits(&mut bit_pos, 1)? != 0;
        if equal_picture_interval {
            skip_uvlc(&mut bit_pos)?;
        }
        let decoder_model_info_present = read_bits(&mut bit_pos, 1)? != 0;
        if decoder_model_info_present {
            let _buffer_delay_length_minus_1 = read_bits(&mut bit_pos, 5)?;
            let _num_units_in_decoding_tick = read_bits(&mut bit_pos, 32)?;
            let _buffer_removal_time_length_minus_1 = read_bits(&mut bit_pos, 5)?;
            let _frame_presentation_time_length_minus_1 = read_bits(&mut bit_pos, 5)?;
        }
    }
    let _initial_display_delay_present = read_bits(&mut bit_pos, 1)?;
    let _operating_points_cnt_minus_1 = read_bits(&mut bit_pos, 5)?;
    let _operating_point_idc_0 = read_bits(&mut bit_pos, 12)?;
    let seq_level_idx_0 = read_bits(&mut bit_pos, 5)?;
    if seq_level_idx_0 > 7 { Some(read_bits(&mut bit_pos, 1)? != 0) } else { Some(false) }
}

/// Build an `av1C` record from the sequence header parameter bytes NVENC wrote.
pub fn av1_config(sequence_params: &[u8], ten_bit: bool) -> Result<Av1Config, String> {
    if sequence_params.is_empty() {
        return Err("empty AV1 sequence parameters".into());
    }
    let (full_obu, payload) = extract_sequence_header_obu(sequence_params).ok_or_else(|| "no Sequence Header OBU found in NVENC parameters".to_string())?;

    let parsed = SequenceHeader::parse(payload).map_err(|e| format!("parsing AV1 sequence header: {e:?}"))?;
    let seq_tier_0 = parse_seq_tier_0(payload).ok_or_else(|| "parsing AV1 sequence header tier".to_string())?;

    if parsed.color.bit_depth != if ten_bit { 10 } else { 8 } {
        return Err("NVENC AV1 sequence bit depth differs from the input configuration".into());
    }
    let high_bitdepth = parsed.color.bit_depth > 8;
    let config = Av1Config {
        seq_profile: parsed.profile,
        seq_level_idx_0: parsed.seq_level_idx.first().copied().unwrap_or(0),
        seq_tier_0,
        high_bitdepth,
        twelve_bit: parsed.color.bit_depth == 12,
        monochrome: parsed.color.mono_chrome,
        chroma_subsampling_x: parsed.color.subsampling_x != 0,
        chroma_subsampling_y: parsed.color.subsampling_y != 0,
        chroma_sample_position: parsed.color.chroma_sample_position,
        initial_presentation_delay_minus_one: None,
        config_obus: full_obu.to_vec(),
    };

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_obu_lengths_and_headers_are_rejected() {
        for bytes in [vec![0x0a, 0x80], vec![0x0a, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0], vec![0x8a, 0], vec![0x0b, 0], vec![0x0a, 4, 0]] {
            assert!(std::panic::catch_unwind(|| extract_sequence_header_obu(&bytes)).unwrap().is_none());
        }
    }

    #[test]
    fn hdr_metadata_converts_decimal_gbr_to_binary_rgb() {
        let display = filmcraft_isobmff::MasteringDisplay::bt2020(1000.0, 0.005);
        let signal = super::super::Signal { sei: vec![super::super::Sei { payload_type: 137, payload: display.to_bytes() }], ..Default::default() };
        let bytes = hdr_metadata(&signal).unwrap();
        assert_eq!(&bytes[..3], &[0x2a, 26, 2]);
        let red = u16::from_be_bytes(bytes[3..5].try_into().unwrap());
        assert_eq!(red, 46399);
        assert_eq!(u32::from_be_bytes(bytes[19..23].try_into().unwrap()), 256000);
        assert_eq!(u32::from_be_bytes(bytes[23..27].try_into().unwrap()), 82);
        assert_eq!(bytes[27], 0x80);
        for size in [0, 1, 23, 25] {
            let signal = super::super::Signal { sei: vec![super::super::Sei { payload_type: 137, payload: vec![0; size] }], ..Default::default() };
            assert!(std::panic::catch_unwind(|| hdr_metadata(&signal)).unwrap().is_err());
        }
        assert!(insert_metadata(&[0x0a, 0xff], &bytes).is_err());
        assert_eq!(insert_metadata(&[0x12, 0, 0x32, 1, 0], &bytes).unwrap(), [vec![0x12, 0], bytes, vec![0x32, 1, 0]].concat());
    }

    #[test]
    fn test_extract_and_parse_synthetic_av1_config() {
        // A minimal AV1 Sequence Header OBU (Profile 0 Main, level 0, 8-bit 4:2:0)
        // OBU Header: type 1 (seq header), no ext, has_size: (1 << 3) | 2 = 0x0a
        // LEB128 size: 4 bytes
        // payload: profile 0 (3 bits: 0), still_picture (1 bit: 0), reduced_still_picture (1 bit: 0),
        // timing_info_present (1 bit: 0), initial_display_delay (1 bit: 0), op_points_cnt_minus_1 (5 bits: 0), ...
        let mut obu = vec![0x0a];
        let payload = vec![0x00, 0x00, 0x20, 0x00];
        obu.push(payload.len() as u8);
        obu.extend_from_slice(&payload);

        let parsed = extract_sequence_header_obu(&obu);
        assert!(parsed.is_some());
        let (full, pay) = parsed.unwrap();
        assert_eq!(full, &obu[..]);
        assert_eq!(pay, &payload[..]);
    }

    #[test]
    fn av1_config_parses_seq_tier_1() {
        let mut bits = Vec::new();
        let mut push = |val: u32, n: usize| {
            for i in (0..n).rev() {
                bits.push(((val >> i) & 1) as u8);
            }
        };
        push(0, 3); // seq_profile = 0 (Main)
        push(0, 1); // still_picture = 0
        push(0, 1); // reduced_still_picture_header = 0
        push(0, 1); // timing_info_present_flag = 0
        push(0, 1); // initial_display_delay_present_flag = 0
        push(0, 5); // operating_points_cnt_minus_1 = 0
        push(0, 12); // operating_point_idc[0] = 0
        push(8, 5); // seq_level_idx[0] = 8 (Level 4.0, > 7 so seq_tier follows)
        push(1, 1); // seq_tier[0] = 1
        push(0, 4); // frame_width_bits_minus_1 = 0
        push(0, 4); // frame_height_bits_minus_1 = 0
        push(0, 1); // max_frame_width_minus_1 = 0
        push(0, 1); // max_frame_height_minus_1 = 0
        push(0, 1); // frame_id_numbers_present_flag = 0
        push(0, 1); // use_128x128_superblock = 0
        push(0, 1); // enable_filter_intra = 0
        push(0, 1); // enable_intra_edge_filter = 0
        push(0, 1); // enable_interintra_compound = 0
        push(0, 1); // enable_masked_compound = 0
        push(0, 1); // enable_warped_motion = 0
        push(0, 1); // enable_dual_filter = 0
        push(0, 1); // enable_order_hint = 0
        push(1, 1); // seq_choose_screen_content_tools = 1
        push(1, 1); // seq_choose_integer_mv = 1
        push(0, 1); // enable_superres = 0
        push(0, 1); // enable_cdef = 0
        push(0, 1); // enable_restoration = 0
        push(0, 1); // color_config: high_bitdepth = 0
        push(0, 1); // mono_chrome = 0
        push(0, 1); // color_description_present_flag = 0
        push(0, 1); // color_range = 0
        push(0, 2); // chroma_sample_position = 0
        push(0, 1); // separate_uv_delta_q = 0
        push(0, 1); // film_grain_params_present = 0
        push(1, 1); // trailing_one_bit
        while bits.len() % 8 != 0 {
            bits.push(0);
        }
        let payload: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0u8, |acc, &b| (acc << 1) | b)).collect();
        let mut obu = vec![0x0a, payload.len() as u8];
        obu.extend_from_slice(&payload);

        let cfg = av1_config(&obu, false).expect("valid AV1 sequence header with tier 1");
        assert_eq!(cfg.seq_level_idx_0, 8);
        assert!(cfg.seq_tier_0);
    }
}

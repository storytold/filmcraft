//! What is specific to AV1 in the NVENC backend (safe code): OBU stream framing for ISOBMFF
//! samples, and the `av1C` codec configuration record built from the sequence header the encoder
//! wrote.
//!
//! Main (8-bit 4:2:0) is written; the record is refused if the sequence header disagrees with the
//! requested profile, bit depth, subsampling, or size.

use craft_video::av1::decode::SequenceHeader;
use filmcraft_isobmff::Av1Config;

/// OBU types (AV1 Bitstream Spec §6.2.2).
const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;
const OBU_PADDING: u8 = 15;

struct ParsedObu<'a> {
    obu_type: u8,
    ext: Option<u8>,
    has_size: bool,
    full: &'a [u8],
    payload: &'a [u8],
}

/// Encode `len` as unsigned LEB128 (up to 8 bytes).
fn write_leb128(mut len: usize, out: &mut Vec<u8>) {
    loop {
        let byte = (len & 0x7f) as u8;
        len >>= 7;
        if len == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Split a low-overhead AV1 OBU byte stream into its OBUs.
fn split_obus(data: &[u8]) -> Result<Vec<ParsedObu<'_>>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let start = pos;
        let Some(&h) = data.get(pos) else { break };
        if h & 0x80 != 0 {
            return Err("AV1 OBU has forbidden bit set".into());
        }
        let obu_type = (h >> 3) & 0x0f;
        let has_ext = h & 0x04 != 0;
        let has_size = h & 0x02 != 0;
        pos += 1;
        let ext = if has_ext {
            let b = *data.get(pos).ok_or_else(|| "truncated AV1 OBU extension header".to_string())?;
            pos += 1;
            Some(b)
        } else {
            None
        };
        let size = if has_size {
            let mut val = 0usize;
            let mut shift = 0u32;
            let mut count = 0usize;
            loop {
                let &b = data.get(pos).ok_or_else(|| "truncated AV1 OBU size".to_string())?;
                pos += 1;
                count += 1;
                let low = usize::from(b & 0x7f);
                let term = low.checked_shl(shift).ok_or_else(|| "AV1 OBU size overflows usize".to_string())?;
                val |= term;
                if b & 0x80 == 0 {
                    break;
                }
                if count >= 8 {
                    return Err("AV1 OBU LEB128 size exceeds 8 bytes".into());
                }
                shift += 7;
            }
            val
        } else {
            data.len().saturating_sub(pos)
        };
        let end = pos.checked_add(size).ok_or_else(|| "AV1 OBU size overflows slice".to_string())?;
        let payload = data.get(pos..end).ok_or_else(|| "truncated AV1 OBU payload".to_string())?;
        let full = data.get(start..end).ok_or_else(|| "truncated AV1 OBU".to_string())?;
        out.push(ParsedObu { obu_type, ext, has_size, full, payload });
        pos = end;
    }
    Ok(out)
}

fn push_sized_obu(obu: &ParsedObu<'_>, out: &mut Vec<u8>) {
    if obu.has_size {
        out.extend_from_slice(obu.full);
        return;
    }
    let header = (obu.full.first().copied().unwrap_or(0) & 0xfe) | 0x02;
    out.push(header);
    if let Some(ext) = obu.ext {
        out.push(ext);
    }
    write_leb128(obu.payload.len(), out);
    out.extend_from_slice(obu.payload);
}

/// Format an encoded AV1 temporal unit as an ISOBMFF `av01` sample: strip temporal delimiters,
/// padding, and redundant frame headers (AV1-ISOBMFF §2.4), and ensure every OBU carries a size
/// field (`obu_has_size_field = 1`).
pub fn av1_obu_sample(data: &[u8]) -> Result<Vec<u8>, String> {
    let obus = split_obus(data)?;
    if obus.is_empty() {
        return Err("the encoder produced no OBUs".into());
    }
    let mut out = Vec::with_capacity(data.len());
    for obu in &obus {
        if matches!(obu.obu_type, OBU_TEMPORAL_DELIMITER | OBU_REDUNDANT_FRAME_HEADER | OBU_PADDING) {
            continue;
        }
        push_sized_obu(obu, &mut out);
    }
    if out.is_empty() {
        return Err("the encoder produced no AV1 picture data".into());
    }
    Ok(out)
}

/// Minimal MSB-first bit reader over an AV1 OBU payload.
struct Bits<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bit(&mut self) -> Result<u32, String> {
        let byte_idx = self.bit_pos / 8;
        let bit_idx = 7 - (self.bit_pos % 8);
        let &b = self.data.get(byte_idx).ok_or_else(|| "truncated AV1 sequence header".to_string())?;
        self.bit_pos += 1;
        Ok(u32::from((b >> bit_idx) & 1))
    }

    fn read_bits(&mut self, n: u8) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()?;
        }
        Ok(v)
    }

    fn skip_bits(&mut self, n: usize) -> Result<(), String> {
        let next = self.bit_pos.checked_add(n).ok_or_else(|| "AV1 sequence header bit offset overflow".to_string())?;
        if next > self.data.len().saturating_mul(8) {
            return Err("truncated AV1 sequence header".into());
        }
        self.bit_pos = next;
        Ok(())
    }

    fn skip_uvlc(&mut self) -> Result<(), String> {
        let mut leading_zeros = 0usize;
        while leading_zeros < 32 {
            if self.read_bit()? != 0 {
                break;
            }
            leading_zeros += 1;
        }
        if leading_zeros >= 32 {
            return Ok(());
        }
        self.skip_bits(leading_zeros)
    }
}

/// Parse operating point 0's `seq_level_idx`, `seq_tier`, and `initial_display_delay_minus_1` from
/// an AV1 `sequence_header_obu()` payload (AV1 Spec §5.5.1).
fn parse_op0_level_tier(seq_payload: &[u8]) -> Result<(u8, bool, Option<u8>), String> {
    let mut r = Bits::new(seq_payload);
    let _seq_profile = r.read_bits(3)?;
    let _still_picture = r.read_bit()?;
    let reduced_still_picture_header = r.read_bit()? != 0;
    if reduced_still_picture_header {
        let level_idx = r.read_bits(5)? as u8;
        return Ok((level_idx, false, None));
    }
    let timing_info_present_flag = r.read_bit()? != 0;
    let mut decoder_model_info_present_flag = false;
    let mut buffer_delay_length_minus_1 = 0u32;
    if timing_info_present_flag {
        r.skip_bits(32)?; // num_units_in_display_tick
        r.skip_bits(32)?; // time_scale
        let equal_picture_interval = r.read_bit()? != 0;
        if equal_picture_interval {
            r.skip_uvlc()?; // num_ticks_per_picture_minus_1
        }
        decoder_model_info_present_flag = r.read_bit()? != 0;
        if decoder_model_info_present_flag {
            buffer_delay_length_minus_1 = r.read_bits(5)?;
            r.skip_bits(32)?; // num_units_in_decoding_tick
            r.skip_bits(5)?; // buffer_removal_time_length_minus_1
            r.skip_bits(5)?; // frame_presentation_time_length_minus_1
        }
    }
    let initial_display_delay_present_flag = r.read_bit()? != 0;
    let _operating_points_cnt_minus_1 = r.read_bits(5)?;
    // Read operating point 0:
    let _operating_point_idc_0 = r.read_bits(12)?;
    let seq_level_idx_0 = r.read_bits(5)? as u8;
    let seq_tier_0 = if seq_level_idx_0 > 7 { r.read_bit()? != 0 } else { false };
    if decoder_model_info_present_flag && r.read_bit()? != 0 {
        let n = (buffer_delay_length_minus_1 as usize) + 1;
        r.skip_bits(n)?; // decoder_buffer_delay
        r.skip_bits(n)?; // encoder_buffer_delay
        r.skip_bits(1)?; // low_delay_mode_flag
    }
    let initial_presentation_delay_minus_one = if initial_display_delay_present_flag && r.read_bit()? != 0 { Some(r.read_bits(4)? as u8) } else { None };
    Ok((seq_level_idx_0, seq_tier_0, initial_presentation_delay_minus_one))
}

/// Build the `av1C` record for a stream of `size` pictures (width, height) whose sequence OBUs
/// were returned by `nvEncGetSequenceParams`.
pub fn av1_config(seq_obus: &[u8], size: (u32, u32), bit_depth: u8) -> Result<Av1Config, String> {
    if bit_depth != 8 {
        return Err(format!("{bit_depth}-bit AV1 is not written by this backend"));
    }
    let obus = split_obus(seq_obus)?;
    let seq = obus.into_iter().find(|o| o.obu_type == OBU_SEQUENCE_HEADER).ok_or_else(|| "the encoder returned no AV1 sequence header".to_string())?;
    let parsed = SequenceHeader::parse(seq.payload).map_err(|e| format!("unreadable AV1 sequence header: {e}"))?;
    if parsed.profile != 0 {
        return Err(format!("the encoder wrote AV1 profile {}, not Main (0)", parsed.profile));
    }
    let cc = &parsed.color;
    if cc.mono_chrome || cc.subsampling_x != 1 || cc.subsampling_y != 1 || cc.bit_depth != bit_depth {
        return Err(format!(
            "the encoder wrote {}-bit subsampling ({},{}) mono={}, not {bit_depth}-bit 4:2:0",
            cc.bit_depth, cc.subsampling_x, cc.subsampling_y, cc.mono_chrome
        ));
    }
    let actual = (parsed.max_frame_width, parsed.max_frame_height);
    if actual != size {
        return Err(format!("the encoder wrote {}x{} pictures for a {}x{} export", actual.0, actual.1, size.0, size.1));
    }
    let (seq_level_idx_0, seq_tier_0, initial_presentation_delay_minus_one) = parse_op0_level_tier(seq.payload)?;
    let mut config_obus = Vec::with_capacity(seq.payload.len() + 4);
    push_sized_obu(&seq, &mut config_obus);
    Ok(Av1Config {
        seq_profile: parsed.profile,
        seq_level_idx_0,
        seq_tier_0,
        high_bitdepth: false,
        twelve_bit: false,
        monochrome: cc.mono_chrome,
        chroma_subsampling_x: cc.subsampling_x != 0,
        chroma_subsampling_y: cc.subsampling_y != 0,
        chroma_sample_position: 0,
        initial_presentation_delay_minus_one,
        config_obus,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1280x720 10-bit 4:2:0 SVT-AV1 sequence header OBU (Main profile, 10-bit, level 3.1).
    const SEQ_OBU_1280X720_10: [u8; 14] = [0x0a, 0x0c, 0x00, 0x00, 0x00, 0x2d, 0x6a, 0x67, 0xfd, 0x9e, 0x01, 0x7c, 0x20, 0x20];

    fn make_seq_obu_8bit() -> Vec<u8> {
        // Construct a minimal valid AV1 sequence header OBU payload for 1280x720 8-bit 4:2:0:
        // seq_profile=0 (3 bits), still_picture=0 (1), reduced_still_picture_header=1 (1),
        // seq_level_idx[0]=5 (5 bits -> level 3.1),
        // frame_width_bits_minus_1=10 (4 bits -> 11 bits), frame_height_bits_minus_1=9 (4 bits -> 10 bits),
        // max_frame_width_minus_1=1279 (11 bits), max_frame_height_minus_1=719 (10 bits),
        // use_128x128_superblock=0 (1), enable_filter_intra=0 (1), enable_intra_edge_filter=0 (1),
        // [not reduced ->] (reduced=1 skips timing/inter/sb-detail!)
        // In reduced_still_picture_header=1:
        // seq_profile(3)=000, still_picture(1)=0, reduced(1)=1, seq_level_idx(5)=00101,
        // frame_width_bits_minus_1(4)=1010 (10->11bits), frame_height_bits_minus_1(4)=1001 (9->10bits),
        // max_frame_width_minus_1(11)=10011111111 (1279), max_frame_height_minus_1(10)=1011001111 (719),
        // use_128x128_superblock(1)=0, enable_filter_intra(1)=0, enable_intra_edge_filter(1)=0,
        // enable_superres(1)=0, enable_cdef(1)=0, enable_restoration(1)=0,
        // color_config: high_bitdepth(1)=0, mono_chrome(1)=0, color_description_present_flag(1)=0,
        // color_range(1)=0, chroma_sample_position(2)=00, separate_uv_delta_q(1)=0,
        // film_grain_params_present(1)=0, trailing bit = 1 + zeros.
        let bits = "00001\
                    00101\
                    10101001\
                    10011111111\
                    1011001111\
                    000\
                    000\
                    0000000\
                    0\
                    100";
        assert_eq!(bits.len() % 8, 0);
        let payload: Vec<u8> = bits.as_bytes().chunks_exact(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap()).collect();
        let mut obu = vec![0x0a];
        write_leb128(payload.len(), &mut obu);
        obu.extend_from_slice(&payload);
        obu
    }

    #[test]
    fn splits_and_formats_av1_sample_stripping_temporal_delimiters() {
        let td = [0x12u8, 0x00]; // OBU_TEMPORAL_DELIMITER (type=2, has_size=1, size=0)
        let seq = make_seq_obu_8bit();
        let frame_unsized = [0x30u8, 0xaa, 0xbb, 0xcc]; // OBU_FRAME (type=6, has_size=0)
        let pad = [0x7au8, 0x01, 0xff]; // OBU_PADDING (type=15, has_size=1, size=1)

        // Check sized OBUs strip TD and padding, keep seq + frame, and add size field if missing.
        let mut stream = Vec::new();
        stream.extend_from_slice(&td);
        stream.extend_from_slice(&seq);
        // Frame with size field: header 0x32 (type=6, has_size=1), len=3, payload 0xaa 0xbb 0xcc
        stream.extend_from_slice(&[0x32, 0x03, 0xaa, 0xbb, 0xcc]);
        stream.extend_from_slice(&pad);

        let sample = av1_obu_sample(&stream).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&seq);
        expected.extend_from_slice(&[0x32, 0x03, 0xaa, 0xbb, 0xcc]);
        assert_eq!(sample, expected);

        // Unsized trailing frame OBU gets converted to sized.
        let mut unsized_stream = Vec::new();
        unsized_stream.extend_from_slice(&td);
        unsized_stream.extend_from_slice(&frame_unsized);
        let sample2 = av1_obu_sample(&unsized_stream).unwrap();
        assert_eq!(sample2, &[0x32, 0x03, 0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn builds_av1_config_from_sequence_header_and_rejects_mismatches() {
        let seq = make_seq_obu_8bit();
        // Verify our constructed OBU parses cleanly and builds a valid Av1Config.
        let cfg = av1_config(&seq, (1280, 720), 8).unwrap();
        assert_eq!(cfg.seq_profile, 0);
        assert_eq!(cfg.seq_level_idx_0, 5);
        assert!(!cfg.seq_tier_0);
        assert!(!cfg.high_bitdepth);
        assert!(!cfg.twelve_bit);
        assert!(!cfg.monochrome);
        assert!(cfg.chroma_subsampling_x);
        assert!(cfg.chroma_subsampling_y);
        assert_eq!(cfg.config_obus, seq);
        assert_eq!(Av1Config::parse(&cfg.to_bytes()).unwrap(), cfg);

        // Wrong size or depth is refused.
        assert!(av1_config(&seq, (1920, 1080), 8).unwrap_err().contains("1280x720"));
        assert!(av1_config(&seq, (1280, 720), 10).is_err());

        // Hostile / truncated OBUs never panic.
        let mut td_only = vec![0x12, 0x00];
        assert!(av1_config(&td_only, (1280, 720), 8).is_err());
        assert!(av1_obu_sample(&td_only).is_err());
        assert!(av1_obu_sample(&[]).is_err());
        td_only[0] = 0x80; // forbidden bit set
        assert!(av1_obu_sample(&td_only).is_err());
        for n in 0..seq.len() {
            let r = std::panic::catch_unwind(|| av1_config(&seq[..n], (1280, 720), 8));
            assert!(r.is_ok(), "panic on truncation at {n}");
            assert!(r.unwrap().is_err(), "accepted truncated seq at {n}");
        }
        // Check that SEQ_OBU_1280X720_10 (10-bit SVT-AV1 sample) is rejected for 8-bit Main.
        assert!(av1_config(&SEQ_OBU_1280X720_10, (1280, 720), 8).is_err());
    }
}

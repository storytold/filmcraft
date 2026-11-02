//! Minimal WAVE (RIFF) and AIFF containers for audio essence embedded in AAF / OMF documents.
//! PCM inside the model is interleaved little-endian signed integers (8-bit unsigned for WAVE).

/// A complete WAVE file around little-endian PCM.
pub fn wav_file(pcm: &[u8], channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let mut v = wav_header(pcm.len() as u32, channels, sample_rate, bits);
    v.extend_from_slice(pcm);
    if pcm.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// The RIFF header of a WAVE file up to and including the `data` chunk header.
pub fn wav_header(data_len: u32, channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let block = channels.max(1) * bits.div_ceil(8);
    let mut v = Vec::with_capacity(44);
    v.extend_from_slice(b"RIFF");
    // Saturating: a huge source (data_len saturated by the caller) must not overflow.
    v.extend_from_slice(&36u32.saturating_add(data_len).saturating_add(data_len & 1).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.max(1).to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&sample_rate.saturating_mul(block as u32).to_le_bytes());
    v.extend_from_slice(&block.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v
}

/// PCM, channels, sample rate and bits of a WAVE file (integer PCM only).
pub fn parse_wav(b: &[u8]) -> Option<(Vec<u8>, u16, u32, u16)> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut fmt = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().ok()?) as usize;
        let body = &b[at + 8..(at + 8).saturating_add(len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            let tag = u16::from_le_bytes([body[0], body[1]]);
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let sr = u32::from_le_bytes(body[4..8].try_into().ok()?);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            if tag != 1 && tag != 0xFFFE {
                return None;
            }
            if tag == 0xFFFE {
                // The first two bytes of the sub-format GUID carry the real format code.
                let sub = body.get(24..26)?;
                if u16::from_le_bytes([sub[0], sub[1]]) != 1 {
                    return None;
                }
            }
            fmt = Some((ch, sr, bits));
        } else if id == b"data" {
            let (ch, sr, bits) = fmt?;
            return Some((body.to_vec(), ch, sr, bits));
        }
        at = at.checked_add(8 + len + (len & 1))?;
    }
    None
}

/// A complete AIFF file (big-endian PCM) from little-endian PCM.
pub fn aiff_file(pcm: &[u8], channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let bps = bits.div_ceil(8) as usize;
    let frames = pcm.len() / (bps * channels.max(1) as usize).max(1);
    let mut v = aiff_header(frames as u32, channels, sample_rate, bits);
    for s in pcm.chunks_exact(bps.max(1)) {
        if bps == 1 {
            v.push(s[0] ^ 0x80); // WAVE 8-bit is unsigned, AIFF signed
        } else {
            v.extend(s.iter().rev());
        }
    }
    if (v.len() - 8) % 2 == 1 {
        v.push(0);
    }
    v
}

/// The AIFF header up to the sound data (`FORM`, `COMM`, `SSND` with offset and block size).
pub fn aiff_header(frames: u32, channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let data_len = frames as usize * channels.max(1) as usize * bits.div_ceil(8) as usize;
    let mut v = Vec::with_capacity(54);
    v.extend_from_slice(b"FORM");
    v.extend_from_slice(&((4 + 26 + 16 + data_len + (data_len & 1)) as u32).to_be_bytes());
    v.extend_from_slice(b"AIFFCOMM");
    v.extend_from_slice(&18u32.to_be_bytes());
    v.extend_from_slice(&channels.max(1).to_be_bytes());
    v.extend_from_slice(&frames.to_be_bytes());
    v.extend_from_slice(&bits.to_be_bytes());
    v.extend_from_slice(&extended(sample_rate as f64));
    v.extend_from_slice(b"SSND");
    v.extend_from_slice(&((8 + data_len) as u32).to_be_bytes());
    v.extend_from_slice(&[0; 8]);
    v
}

/// PCM (converted to little-endian), channels, sample rate and bits of an AIFF / AIFF-C (`NONE`
/// / `twos` / `sowt`) file.
pub fn parse_aiff(b: &[u8]) -> Option<(Vec<u8>, u16, u32, u16)> {
    if b.len() < 12 || &b[..4] != b"FORM" || !(&b[8..12] == b"AIFF" || &b[8..12] == b"AIFC") {
        return None;
    }
    let mut at = 12;
    let mut comm = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_be_bytes(b[at + 4..at + 8].try_into().ok()?) as usize;
        let body = &b[at + 8..(at + 8).saturating_add(len).min(b.len())];
        if id == b"COMM" && body.len() >= 18 {
            let ch = u16::from_be_bytes([body[0], body[1]]);
            let bits = u16::from_be_bytes([body[6], body[7]]);
            let sr = from_extended(body[8..18].try_into().ok()?).round() as u32;
            let little = body.len() >= 22 && &body[18..22] == b"sowt";
            comm = Some((ch, sr, bits, little));
        } else if id == b"SSND" && body.len() >= 8 {
            let (ch, sr, bits, little) = comm?;
            let off = u32::from_be_bytes(body[..4].try_into().ok()?) as usize;
            let data = body.get(8 + off..)?;
            let bps = bits.div_ceil(8) as usize;
            let mut pcm = Vec::with_capacity(data.len());
            for s in data.chunks_exact(bps.max(1)) {
                if bps == 1 {
                    pcm.push(s[0] ^ 0x80);
                } else if little {
                    pcm.extend_from_slice(s);
                } else {
                    pcm.extend(s.iter().rev());
                }
            }
            return Some((pcm, ch, sr, bits));
        }
        at = at.checked_add(8 + len + (len & 1))?;
    }
    None
}

/// 80-bit IEEE 754 extended (AIFF sample rates).
fn extended(x: f64) -> [u8; 10] {
    let mut out = [0u8; 10];
    if x <= 0.0 {
        return out;
    }
    let e = x.log2().floor() as i32;
    let m = (x / 2f64.powi(e) * (1u64 << 63) as f64) as u64;
    out[..2].copy_from_slice(&((e + 16383) as u16).to_be_bytes());
    out[2..].copy_from_slice(&m.to_be_bytes());
    out
}

fn from_extended(b: [u8; 10]) -> f64 {
    let e = (u16::from_be_bytes([b[0], b[1]]) & 0x7FFF) as i32;
    let m = u64::from_be_bytes(b[2..10].try_into().unwrap_or([0; 8]));
    if e == 0 && m == 0 {
        return 0.0;
    }
    m as f64 / (1u64 << 63) as f64 * 2f64.powi(e - 16383)
}

#[cfg(test)]
mod tests {
    /// A mutated EDL whose clip ran for days produced an OMF descriptor for more than 4 GiB of
    /// PCM: the RIFF size field overflowed ("attempt to add with overflow").
    #[test]
    fn huge_wav_header_saturates() {
        let h = super::wav_header(u32::MAX, 2, u32::MAX, 24);
        assert_eq!(&h[4..8], &u32::MAX.to_le_bytes());
    }

    use super::*;

    #[test]
    fn wav_and_aiff_round_trip() {
        let pcm: Vec<u8> = (0..48u8).collect(); // 8 stereo 24-bit frames
        let w = wav_file(&pcm, 2, 48_000, 24);
        assert_eq!(parse_wav(&w), Some((pcm.clone(), 2, 48_000, 24)));
        let a = aiff_file(&pcm, 2, 44_100, 24);
        assert_eq!(parse_aiff(&a), Some((pcm.clone(), 2, 44_100, 24)));
        let p16: Vec<u8> = (0..32u8).collect();
        assert_eq!(parse_aiff(&aiff_file(&p16, 1, 96_000, 16)), Some((p16, 1, 96_000, 16)));
        assert!(parse_wav(b"RIFF").is_none());
        assert!(parse_aiff(&a[..30]).is_none());
        for cut in 0..w.len() {
            let _ = parse_wav(&w[..cut]);
            let _ = parse_aiff(&a[..cut.min(a.len())]);
        }
    }
}

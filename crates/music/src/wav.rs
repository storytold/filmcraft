//! Minimal RIFF/WAVE reader for provider responses (PCM 16/24/32-bit and 32-bit float).

use crate::{Audio, MusicError};

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

pub fn decode(bytes: &[u8]) -> Result<Audio, MusicError> {
    let bad = |m: &str| MusicError::Inference(format!("bad WAV: {m}"));
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let mut pos = 12usize;
    let (mut fmt, mut data): (Option<(u16, u16, u32, u16)>, Option<&[u8]>) = (None, None);
    while let (Some(id), Some(len)) = (bytes.get(pos..pos.saturating_add(4)), u32_at(bytes, pos.saturating_add(4))) {
        let start = pos.saturating_add(8);
        let end = start.saturating_add(len as usize).min(bytes.len());
        let body = bytes.get(start..end).unwrap_or(&[]);
        match id {
            b"fmt " => {
                let f = |i| u16_at(body, i).ok_or_else(|| bad("short fmt chunk"));
                fmt = Some((f(0)?, f(2)?, u32_at(body, 4).ok_or_else(|| bad("short fmt chunk"))?, f(14)?));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos = start.saturating_add(len as usize).saturating_add(len as usize & 1);
    }
    let (tag, channels, rate, bits) = fmt.ok_or_else(|| bad("missing fmt chunk"))?;
    let data = data.ok_or_else(|| bad("missing data chunk"))?;
    if channels == 0 || rate == 0 {
        return Err(bad("zero channels or sample rate"));
    }
    let samples: Vec<f32> = match (tag, bits) {
        (1, 16) => data.as_chunks::<2>().0.iter().map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0).collect(),
        (1, 24) => data.as_chunks::<3>().0.iter().map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_608.0).collect(),
        (1, 32) => data.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c) as f32 / 2_147_483_648.0).collect(),
        (3, 32) => data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect(),
        _ => return Err(bad("unsupported sample format")),
    };
    Ok(Audio { samples, sample_rate: rate, channels })
}

/// Encode interleaved samples as a 32-bit float WAV.
pub fn encode_f32(a: &Audio) -> Vec<u8> {
    let data_len = u32::try_from(a.samples.len().saturating_mul(4)).unwrap_or(u32::MAX);
    let block = u32::from(a.channels).saturating_mul(4);
    let mut o = Vec::with_capacity(44usize.saturating_add(a.samples.len().saturating_mul(4)));
    o.extend(b"RIFF");
    o.extend(data_len.saturating_add(36).to_le_bytes());
    o.extend(b"WAVEfmt ");
    o.extend(16u32.to_le_bytes());
    o.extend(3u16.to_le_bytes());
    o.extend(a.channels.to_le_bytes());
    o.extend(a.sample_rate.to_le_bytes());
    o.extend(a.sample_rate.saturating_mul(block).to_le_bytes());
    o.extend(u16::try_from(block).unwrap_or(u16::MAX).to_le_bytes());
    o.extend(32u16.to_le_bytes());
    o.extend(b"data");
    o.extend(data_len.to_le_bytes());
    for s in &a.samples {
        o.extend(s.to_le_bytes());
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_roundtrip() {
        let a = Audio { samples: vec![0.0, 0.5, -0.5, 1.0], sample_rate: 44_100, channels: 2 };
        assert_eq!(decode(&encode_f32(&a)).unwrap(), a);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"nope").is_err());
        assert!(decode(&[]).is_err());
    }
}

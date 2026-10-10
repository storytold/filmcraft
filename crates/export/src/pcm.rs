//! Audio-only files: WAV (RIFF, little-endian PCM) and AIFF (big-endian PCM), 16 or 24 bit, plus
//! image-sequence file naming.

/// Quantise one sample to a signed integer of `bits` (16 or 24).
fn quantise(s: f32, bits: u16) -> i32 {
    let max = if bits >= 24 { 8_388_607.0 } else { 32_767.0 };
    (s.clamp(-1.0, 1.0) * max).round() as i32
}

/// A RIFF/WAVE file of interleaved samples. More than two channels are written as
/// `WAVE_FORMAT_EXTENSIBLE` with the speaker mask of the layout (6 channels: L, R, C, LFE, Ls, Rs
/// = `0x3F`).
pub fn write_wav(interleaved: &[f32], channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let bits: u16 = if bits >= 24 { 24 } else { 16 };
    let bps = (bits / 8) as u32;
    let data_len = interleaved.len() as u32 * bps;
    let ext = channels > 2;
    let fmt_len: u32 = if ext { 40 } else { 16 };
    let mut v = Vec::with_capacity(28 + fmt_len as usize + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(20 + fmt_len + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&fmt_len.to_le_bytes());
    v.extend_from_slice(&(if ext { 0xFFFEu16 } else { 1 }).to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&(sample_rate * channels as u32 * bps).to_le_bytes());
    v.extend_from_slice(&(channels * bps as u16).to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    if ext {
        // cbSize, valid bits, channel mask, KSDATAFORMAT_SUBTYPE_PCM
        v.extend_from_slice(&22u16.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        let mask: u32 = if channels == 6 { 0x3F } else { 0 };
        v.extend_from_slice(&mask.to_le_bytes());
        v.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
    }
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for &s in interleaved {
        let q = quantise(s, bits).to_le_bytes();
        v.extend_from_slice(&q[..bps as usize]);
    }
    v
}

/// IEEE 754 80-bit extended encoding of a sample rate (AIFF `COMM`).
fn extended(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let shift = rate.leading_zeros();
    let mant = (rate as u64) << (32 + shift);
    let exp = 16383 + 31 - shift as u16;
    out[..2].copy_from_slice(&exp.to_be_bytes());
    out[2..].copy_from_slice(&mant.to_be_bytes());
    out
}

/// An AIFF file (`FORM`/`AIFF`, `COMM` + `SSND`) of interleaved samples.
pub fn write_aiff(interleaved: &[f32], channels: u16, sample_rate: u32, bits: u16) -> Vec<u8> {
    let bits: u16 = if bits >= 24 { 24 } else { 16 };
    let bps = (bits / 8) as usize;
    let frames = interleaved.len() / channels.max(1) as usize;
    let data_len = (interleaved.len() * bps) as u32;
    let ssnd_len = 8 + data_len;
    let form_len = 4 + (8 + 18) + (8 + ssnd_len) + (ssnd_len & 1);
    let mut v = Vec::with_capacity(form_len as usize + 8);
    v.extend_from_slice(b"FORM");
    v.extend_from_slice(&form_len.to_be_bytes());
    v.extend_from_slice(b"AIFFCOMM");
    v.extend_from_slice(&18u32.to_be_bytes());
    v.extend_from_slice(&channels.to_be_bytes());
    v.extend_from_slice(&(frames as u32).to_be_bytes());
    v.extend_from_slice(&bits.to_be_bytes());
    v.extend_from_slice(&extended(sample_rate));
    v.extend_from_slice(b"SSND");
    v.extend_from_slice(&ssnd_len.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes()); // offset
    v.extend_from_slice(&0u32.to_be_bytes()); // block size
    for &s in interleaved {
        let q = quantise(s, bits).to_be_bytes();
        v.extend_from_slice(&q[4 - bps..]);
    }
    if ssnd_len & 1 == 1 {
        v.push(0);
    }
    v
}

/// File name of frame `index` of an image sequence of `count` frames exported to `path`:
/// the file name's stem followed directly by the zero-padded frame number (at least three
/// digits, more when the count needs them), starting at 000, like Premiere's numbered stills:
/// `Sequence 01.png` → `Sequence 01000.png`, `Sequence 01001.png`, …
pub fn image_sequence_path(path: &str, index: u64, count: u64) -> String {
    let p = std::path::Path::new(path);
    let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "png".into());
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let digits = count.saturating_sub(1).max(1).to_string().len().max(3);
    let name = format!("{stem}{index:0digits$}.{ext}");
    match p.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => d.join(name).to_string_lossy().to_string(),
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_sample_rates() {
        assert_eq!(extended(44_100), [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
        assert_eq!(extended(48_000), [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn sequence_names() {
        use std::path::Path;
        assert_eq!(Path::new(&image_sequence_path("/a/Sequence 01.png", 0, 24)), Path::new("/a/Sequence 01000.png"));
        assert_eq!(Path::new(&image_sequence_path("/a/x.tif", 1234, 2000)), Path::new("/a/x1234.tif"));
        assert_eq!(image_sequence_path("shot.bmp", 7, 10), "shot007.bmp");
    }

    #[test]
    fn wav_and_aiff_layout() {
        let s = [0.5f32, -0.5, 1.0, -1.0];
        let w = write_wav(&s, 2, 48_000, 24);
        assert_eq!(w.len(), 44 + 12);
        assert_eq!(&w[34..36], &24u16.to_le_bytes());
        let a = write_aiff(&s, 2, 48_000, 16);
        assert_eq!(&a[..4], b"FORM");
        assert_eq!(u32::from_be_bytes(a[4..8].try_into().unwrap()) as usize, a.len() - 8);
        assert_eq!(&a[a.len() - 2..], &(-32767i16).to_be_bytes());
    }
}

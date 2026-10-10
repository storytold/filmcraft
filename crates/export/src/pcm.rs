//! Audio-only files: WAV (RIFF, little-endian PCM; RF64 past 4 GiB) and AIFF (big-endian PCM),
//! 16 or 24 bit, plus image-sequence file naming.
//!
//! The header is written first from the known length ([`header`]), then the samples in chunks
//! ([`encode`]) and the pad byte ([`pad`]), so an export streams to disk instead of building the
//! file in memory.

/// The container of an audio-only export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmContainer {
    Wav,
    Aiff,
}

/// Quantise one sample to a signed integer of `bits` (16 or 24).
pub(crate) fn quantise(s: f32, bits: u16) -> i32 {
    let max = if bits >= 24 { 8_388_607.0 } else { 32_767.0 };
    (s.clamp(-1.0, 1.0) * max).round() as i32
}

/// The sample size written for a requested `bits`: 24, else 16.
fn sample_bits(bits: u16) -> u16 {
    if bits >= 24 { 24 } else { 16 }
}

/// Bytes of sample data for `frames` frames, or None when that does not fit 64 bits.
fn data_len(frames: u64, channels: u16, bits: u16) -> Option<u64> {
    frames.checked_mul(u64::from(channels))?.checked_mul(u64::from(sample_bits(bits) / 8))
}

/// The pad byte after an odd-sized data chunk (RIFF and AIFF chunks are word-aligned).
pub fn pad(frames: u64, channels: u16, bits: u16) -> &'static [u8] {
    if data_len(frames, channels, bits).is_some_and(|n| n & 1 == 1) { &[0] } else { &[] }
}

/// Everything before the samples of a file of `frames` frames. A WAV whose RIFF size would not
/// fit 32 bits (4 GiB: 24-bit 5.1 at 96 kHz after about 41 minutes) is written as RF64 (EBU Tech
/// 3306), with the 64-bit sizes in a `ds64` chunk. AIFF has no 64-bit form, so a longer AIFF is
/// an error naming WAV instead.
pub fn header(container: PcmContainer, channels: u16, sample_rate: u32, bits: u16, frames: u64) -> Result<Vec<u8>, String> {
    let too_long = || "the audio is too long to write".to_string();
    let bits = sample_bits(bits);
    let bps = bits / 8;
    let data = data_len(frames, channels, bits).ok_or_else(too_long)?;
    let block_align = channels.checked_mul(bps).ok_or("too many audio channels")?;
    match container {
        PcmContainer::Wav => {
            let byte_rate = u32::try_from(u64::from(sample_rate) * u64::from(block_align)).map_err(|_| "the audio sample rate is too high".to_string())?;
            let ext = channels > 2;
            let fmt_len: u32 = if ext { 40 } else { 16 };
            let padded = data.checked_add(data & 1).ok_or_else(too_long)?;
            // "WAVE" + fmt chunk + data chunk header + samples
            let riff = (4 + 8 + u64::from(fmt_len) + 8).checked_add(padded).ok_or_else(too_long)?;
            let rf64 = riff > u64::from(u32::MAX);
            let mut v = Vec::with_capacity(80);
            if rf64 {
                const DS64: u64 = 8 + 28;
                v.extend_from_slice(b"RF64");
                v.extend_from_slice(&u32::MAX.to_le_bytes());
                v.extend_from_slice(b"WAVEds64");
                v.extend_from_slice(&28u32.to_le_bytes());
                v.extend_from_slice(&riff.checked_add(DS64).ok_or_else(too_long)?.to_le_bytes());
                v.extend_from_slice(&data.to_le_bytes());
                v.extend_from_slice(&frames.to_le_bytes());
                v.extend_from_slice(&0u32.to_le_bytes()); // no table entries
            } else {
                v.extend_from_slice(b"RIFF");
                v.extend_from_slice(&(riff as u32).to_le_bytes());
                v.extend_from_slice(b"WAVE");
            }
            v.extend_from_slice(b"fmt ");
            v.extend_from_slice(&fmt_len.to_le_bytes());
            v.extend_from_slice(&(if ext { 0xFFFEu16 } else { 1 }).to_le_bytes());
            v.extend_from_slice(&channels.to_le_bytes());
            v.extend_from_slice(&sample_rate.to_le_bytes());
            v.extend_from_slice(&byte_rate.to_le_bytes());
            v.extend_from_slice(&block_align.to_le_bytes());
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
            v.extend_from_slice(&(if rf64 { u32::MAX } else { data as u32 }).to_le_bytes());
            Ok(v)
        }
        PcmContainer::Aiff => {
            let aiff_too_long = || "AIFF holds at most 4 GB of audio: export WAV, which has no such limit".to_string();
            let frames32 = u32::try_from(frames).map_err(|_| aiff_too_long())?;
            // SSND: offset + block size + samples
            let ssnd = u32::try_from(data.checked_add(8).ok_or_else(aiff_too_long)?).map_err(|_| aiff_too_long())?;
            let form = (4u32 + (8 + 18)).checked_add(8).and_then(|n| n.checked_add(ssnd)).and_then(|n| n.checked_add(ssnd & 1)).ok_or_else(aiff_too_long)?;
            let mut v = Vec::with_capacity(54);
            v.extend_from_slice(b"FORM");
            v.extend_from_slice(&form.to_be_bytes());
            v.extend_from_slice(b"AIFFCOMM");
            v.extend_from_slice(&18u32.to_be_bytes());
            v.extend_from_slice(&channels.to_be_bytes());
            v.extend_from_slice(&frames32.to_be_bytes());
            v.extend_from_slice(&bits.to_be_bytes());
            v.extend_from_slice(&extended(sample_rate));
            v.extend_from_slice(b"SSND");
            v.extend_from_slice(&ssnd.to_be_bytes());
            v.extend_from_slice(&0u32.to_be_bytes()); // offset
            v.extend_from_slice(&0u32.to_be_bytes()); // block size
            Ok(v)
        }
    }
}

/// Append interleaved samples as `bits`-bit PCM (little-endian for WAV, big-endian for AIFF).
pub fn encode(container: PcmContainer, interleaved: &[f32], bits: u16, out: &mut Vec<u8>) {
    let bits = sample_bits(bits);
    let bps = usize::from(bits / 8);
    out.reserve(interleaved.len().saturating_mul(bps));
    for &s in interleaved {
        let q = quantise(s, bits);
        match container {
            PcmContainer::Wav => out.extend_from_slice(q.to_le_bytes().get(..bps).unwrap_or_default()),
            PcmContainer::Aiff => out.extend_from_slice(q.to_be_bytes().get(4 - bps..).unwrap_or_default()),
        }
    }
}

/// A whole file in memory (short audio; exports stream with [`header`] and [`encode`]).
fn write_whole(container: PcmContainer, interleaved: &[f32], channels: u16, sample_rate: u32, bits: u16) -> Result<Vec<u8>, String> {
    let frames = (interleaved.len() / usize::from(channels.max(1))) as u64;
    let mut v = header(container, channels, sample_rate, bits, frames)?;
    encode(container, interleaved, bits, &mut v);
    v.extend_from_slice(pad(frames, channels, bits));
    Ok(v)
}

/// A RIFF/WAVE file of interleaved samples. More than two channels are written as
/// `WAVE_FORMAT_EXTENSIBLE` with the speaker mask of the layout (6 channels: L, R, C, LFE, Ls, Rs
/// = `0x3F`).
pub fn write_wav(interleaved: &[f32], channels: u16, sample_rate: u32, bits: u16) -> Result<Vec<u8>, String> {
    write_whole(PcmContainer::Wav, interleaved, channels, sample_rate, bits)
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
pub fn write_aiff(interleaved: &[f32], channels: u16, sample_rate: u32, bits: u16) -> Result<Vec<u8>, String> {
    write_whole(PcmContainer::Aiff, interleaved, channels, sample_rate, bits)
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
        let w = write_wav(&s, 2, 48_000, 24).unwrap();
        assert_eq!(w.len(), 44 + 12);
        assert_eq!(&w[34..36], &24u16.to_le_bytes());
        let a = write_aiff(&s, 2, 48_000, 16).unwrap();
        assert_eq!(&a[..4], b"FORM");
        assert_eq!(u32::from_be_bytes(a[4..8].try_into().unwrap()) as usize, a.len() - 8);
        assert_eq!(&a[a.len() - 2..], &(-32767i16).to_be_bytes());
    }

    fn le32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn le64(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    /// 24-bit 5.1 at 96 kHz passes 4 GiB after about 41 minutes; the sizes overflowed `u32`
    /// (a panic in debug builds, a corrupt header in release).
    #[test]
    fn wav_past_4_gib_is_rf64() {
        let frames = 96_000u64 * 60 * 60; // an hour: 6.2 GB
        let h = header(PcmContainer::Wav, 6, 96_000, 24, frames).unwrap();
        let data = frames * 6 * 3;
        assert_eq!(&h[..4], b"RF64");
        assert_eq!(le32(&h, 4), u32::MAX);
        assert_eq!(&h[8..16], b"WAVEds64");
        assert_eq!(le32(&h, 16), 28);
        // the 64-bit RIFF size covers everything after the first 8 bytes
        assert_eq!(le64(&h, 20), h.len() as u64 - 8 + data);
        assert_eq!((le64(&h, 28), le64(&h, 36), le32(&h, 44)), (data, frames, 0));
        assert_eq!(&h[48..52], b"fmt ");
        assert_eq!(&h[h.len() - 8..h.len() - 4], b"data");
        assert_eq!(le32(&h, h.len() - 4), u32::MAX);
    }

    #[test]
    fn wav_up_to_4_gib_stays_riff() {
        // the longest stereo 16-bit RIFF: 36 bytes of header after the size field
        let max = (u64::from(u32::MAX) - 36) / 4;
        let h = header(PcmContainer::Wav, 2, 48_000, 16, max).unwrap();
        assert_eq!(&h[..4], b"RIFF");
        assert_eq!(u64::from(le32(&h, 4)), 36 + max * 4);
        assert_eq!(u64::from(le32(&h, 40)), max * 4);
        assert_eq!(&header(PcmContainer::Wav, 2, 48_000, 16, max + 1).unwrap()[..4], b"RF64");
    }

    #[test]
    fn aiff_past_4_gib_is_refused_not_wrapped() {
        let e = header(PcmContainer::Aiff, 2, 48_000, 24, 48_000 * 60 * 60 * 8).unwrap_err();
        assert!(e.contains("export WAV"), "{e}");
        assert!(header(PcmContainer::Aiff, 2, 48_000, 24, 48_000 * 60 * 60).is_ok());
    }

    #[test]
    fn hostile_sizes_are_errors() {
        for c in [PcmContainer::Wav, PcmContainer::Aiff] {
            assert!(header(c, 2, 48_000, 24, u64::MAX).is_err());
            assert!(header(c, u16::MAX, 48_000, 24, 1).is_err());
        }
        assert!(header(PcmContainer::Wav, 6, u32::MAX, 24, 1).is_err());
    }

    #[test]
    fn odd_data_chunks_are_padded() {
        // one 24-bit mono frame: 3 bytes of data, one pad byte, counted in the RIFF size
        let w = write_wav(&[0.25], 1, 48_000, 24).unwrap();
        assert_eq!((w.len(), le32(&w, 4) as usize, le32(&w, 40)), (48, 40, 3));
        let a = write_aiff(&[0.25], 1, 48_000, 24).unwrap();
        assert_eq!(u32::from_be_bytes(a[4..8].try_into().unwrap()) as usize, a.len() - 8);
        assert!(pad(2, 1, 24).is_empty());
    }

    /// A reader takes the RF64 sizes: ffprobe reports the duration from `ds64` (skipped without it).
    #[test]
    fn ffprobe_reads_rf64_duration() {
        let dir = std::env::temp_dir().join(format!("filmcraft-rf64-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.wav");
        let frames = 48_000u64 * 60 * 60 * 5; // 5 hours of 24-bit stereo: 5.2 GB
        let mut f = header(PcmContainer::Wav, 2, 48_000, 24, frames).unwrap();
        encode(PcmContainer::Wav, &[0.0; 9_600], 24, &mut f);
        std::fs::write(&path, &f).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=format_name,duration", "-of", "default=nw=1"])
            .arg(&path)
            .output();
        let _ = std::fs::remove_dir_all(&dir);
        let Ok(out) = out else { return };
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("format_name=wav"), "{text}");
        let secs: f64 = text.lines().find_map(|l| l.strip_prefix("duration=")).and_then(|d| d.parse().ok()).unwrap_or(0.0);
        assert!((secs - 18_000.0).abs() < 1.0, "{text}");
    }
}

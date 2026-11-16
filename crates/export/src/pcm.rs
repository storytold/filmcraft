//! Audio-only files: WAV (RIFF, little-endian PCM; RF64 past 4 GiB) and AIFF (big-endian PCM),
//! 16, 24 or 32-bit integer or 32-bit float ([`SampleFormat`]), plus image-sequence file naming.
//! Float WAV is `WAVE_FORMAT_IEEE_FLOAT` with a `fact` chunk; float AIFF is AIFF-C (`fl32`).
//!
//! The header is written first from the known length ([`header`]), then the samples in chunks
//! ([`encode`]) and the pad byte ([`pad`]), so an export streams to disk instead of building the
//! file in memory.

use crate::SampleFormat;
pub use craft_audio::pcm_export::PcmContainer;
#[cfg(test)]
use craft_audio::pcm_export::extended;
#[cfg(test)]
const SUBTYPE_TAIL: [u8; 14] = [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71];
#[cfg(test)]
const AIFC_VERSION_1: u32 = 0xA280_5140;
#[cfg(test)]
const FL32_NAME: &[u8] = b"\x1532-bit floating point";

/// Header for the selected PCM container.
pub fn header(container: PcmContainer, channels: u16, rate: u32, format: SampleFormat, frames: u64) -> Result<Vec<u8>, String> {
    craft_audio::pcm_export::header(container, channels, rate, format.shared(), frames)
}
/// Append a block of interleaved samples.
pub fn encode(container: PcmContainer, samples: &[f32], format: SampleFormat, out: &mut Vec<u8>) {
    craft_audio::pcm_export::encode(container, samples, format.shared(), out)
}
/// Chunk alignment padding for the selected format.
pub fn pad(frames: u64, channels: u16, format: SampleFormat) -> &'static [u8] {
    craft_audio::pcm_export::pad(frames, channels, format.shared())
}
/// Encode a complete WAV file.
pub fn write_wav(samples: &[f32], channels: u16, rate: u32, format: SampleFormat) -> Result<Vec<u8>, String> {
    craft_audio::pcm_export::write_wav(samples, channels, rate, format.shared())
}
/// Encode a complete AIFF file.
pub fn write_aiff(samples: &[f32], channels: u16, rate: u32, format: SampleFormat) -> Result<Vec<u8>, String> {
    craft_audio::pcm_export::write_aiff(samples, channels, rate, format.shared())
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

    const I16: SampleFormat = SampleFormat::Int16;
    const I24: SampleFormat = SampleFormat::Int24;
    const I32: SampleFormat = SampleFormat::Int32;
    const F32: SampleFormat = SampleFormat::Float32;

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
        let w = write_wav(&s, 2, 48_000, I24).unwrap();
        assert_eq!(w.len(), 44 + 12);
        assert_eq!(&w[34..36], &24u16.to_le_bytes());
        let a = write_aiff(&s, 2, 48_000, I16).unwrap();
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
        let h = header(PcmContainer::Wav, 6, 96_000, I24, frames).unwrap();
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
        let h = header(PcmContainer::Wav, 2, 48_000, I16, max).unwrap();
        assert_eq!(&h[..4], b"RIFF");
        assert_eq!(u64::from(le32(&h, 4)), 36 + max * 4);
        assert_eq!(u64::from(le32(&h, 40)), max * 4);
        assert_eq!(&header(PcmContainer::Wav, 2, 48_000, I16, max + 1).unwrap()[..4], b"RF64");
    }

    #[test]
    fn aiff_past_4_gib_is_refused_not_wrapped() {
        let e = header(PcmContainer::Aiff, 2, 48_000, I24, 48_000 * 60 * 60 * 8).unwrap_err();
        assert!(e.contains("export WAV"), "{e}");
        assert!(header(PcmContainer::Aiff, 2, 48_000, I24, 48_000 * 60 * 60).is_ok());
    }

    #[test]
    fn hostile_sizes_are_errors() {
        for c in [PcmContainer::Wav, PcmContainer::Aiff] {
            assert!(header(c, 2, 48_000, I24, u64::MAX).is_err());
            assert!(header(c, u16::MAX, 48_000, I24, 1).is_err());
        }
        assert!(header(PcmContainer::Wav, 6, u32::MAX, I24, 1).is_err());
    }

    #[test]
    fn odd_data_chunks_are_padded() {
        // one 24-bit mono frame: 3 bytes of data, one pad byte, counted in the RIFF size
        let w = write_wav(&[0.25], 1, 48_000, I24).unwrap();
        assert_eq!((w.len(), le32(&w, 4) as usize, le32(&w, 40)), (48, 40, 3));
        let a = write_aiff(&[0.25], 1, 48_000, I24).unwrap();
        assert_eq!(u32::from_be_bytes(a[4..8].try_into().unwrap()) as usize, a.len() - 8);
        assert!(pad(2, 1, I24).is_empty());
    }

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn integer_samples_clip_at_full_scale() {
        assert_eq!([1.0, -1.0, 2.0, -2.0, f32::NAN, 0.5].map(|s| I32.quantise(s)), [i32::MAX, -i32::MAX, i32::MAX, -i32::MAX, 0, 1 << 30]);
        assert_eq!([1.0, -1.0, 4.0].map(|s| I24.quantise(s)), [8_388_607, -8_388_607, 8_388_607]);
        assert_eq!([1.0, -1.0, f32::INFINITY].map(|s| I16.quantise(s)), [32_767, -32_767, 32_767]);
        assert_eq!(F32.quantise(0.5), 0);
        assert_eq!([2.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY].map(SampleFormat::float), [2.5, 0.0, f32::MAX, f32::MIN]);
    }

    #[test]
    fn wav_and_aiff_32_bit_integer() {
        let w = write_wav(&[1.0, -1.0], 1, 48_000, I32).unwrap();
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(u16::from_le_bytes([w[20], w[21]]), 1, "integer PCM");
        assert_eq!((u16::from_le_bytes([w[32], w[33]]), u16::from_le_bytes([w[34], w[35]])), (4, 32));
        assert_eq!(&w[44..], [i32::MAX.to_le_bytes(), (-i32::MAX).to_le_bytes()].concat());
        let a = write_aiff(&[1.0, -1.0], 1, 48_000, I32).unwrap();
        assert_eq!(&a[8..12], b"AIFF");
        assert_eq!(u16::from_be_bytes([a[26], a[27]]), 32);
        assert_eq!(&a[a.len() - 8..], [i32::MAX.to_be_bytes(), (-i32::MAX).to_be_bytes()].concat());
    }

    /// Float keeps the mix as it is: peaks above full scale survive.
    #[test]
    fn wav_32_bit_float() {
        let s = [0.25f32, -2.0, 1.5, f32::NAN];
        let w = write_wav(&s, 2, 48_000, F32).unwrap();
        assert_eq!(&w[12..16], b"fmt ");
        assert_eq!(le32(&w, 16), 18);
        assert_eq!(u16::from_le_bytes([w[20], w[21]]), 3, "WAVE_FORMAT_IEEE_FLOAT");
        assert_eq!((u16::from_le_bytes([w[32], w[33]]), u16::from_le_bytes([w[34], w[35]]), u16::from_le_bytes([w[36], w[37]])), (8, 32, 0));
        assert_eq!((&w[38..42], le32(&w, 42), le32(&w, 46)), (&b"fact"[..], 4, 2));
        assert_eq!((&w[50..54], le32(&w, 54)), (&b"data"[..], 16));
        assert_eq!(le32(&w, 4) as usize, w.len() - 8);
        let back: Vec<f32> = w[58..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(back, [0.25, -2.0, 1.5, 0.0]);
        // 5.1: WAVE_FORMAT_EXTENSIBLE with the IEEE float sub-format, still with `fact`
        let w = write_wav(&[0.5; 12], 6, 48_000, F32).unwrap();
        assert_eq!((le32(&w, 16), u16::from_le_bytes([w[20], w[21]])), (40, 0xFFFE));
        assert_eq!(&w[44..60], [&3u16.to_le_bytes()[..], &SUBTYPE_TAIL].concat());
        assert_eq!((&w[60..64], le32(&w, 68)), (&b"fact"[..], 2));
        // RF64: the frame count is in `ds64`
        let h = header(PcmContainer::Wav, 2, 48_000, F32, 48_000 * 60 * 60 * 4).unwrap();
        assert_eq!(&h[..4], b"RF64");
        let fact = h.windows(4).position(|c| c == b"fact").unwrap();
        assert_eq!(le32(&h, fact + 8), u32::MAX);
    }

    #[test]
    fn aiff_32_bit_float_is_aifc() {
        let a = write_aiff(&[0.25, -3.0], 1, 44_100, F32).unwrap();
        assert_eq!((&a[..4], be32(&a, 4) as usize, &a[8..12]), (&b"FORM"[..], a.len() - 8, &b"AIFC"[..]));
        assert_eq!((&a[12..16], be32(&a, 16), be32(&a, 20)), (&b"FVER"[..], 4, AIFC_VERSION_1));
        assert_eq!((&a[24..28], be32(&a, 28)), (&b"COMM"[..], 44));
        assert_eq!((u16::from_be_bytes([a[32], a[33]]), be32(&a, 34), u16::from_be_bytes([a[38], a[39]])), (1, 2, 32));
        assert_eq!(&a[50..54], b"fl32");
        assert_eq!(&a[54..76], FL32_NAME);
        assert_eq!((&a[76..80], be32(&a, 80)), (&b"SSND"[..], 8 + 8));
        assert_eq!(&a[92..], [0.25f32.to_be_bytes(), (-3.0f32).to_be_bytes()].concat());
    }

    /// ffprobe (an external oracle; skipped where it is not installed) reads every new format.
    #[test]
    fn ffprobe_reads_32_bit_formats() {
        let dir = std::env::temp_dir().join(format!("filmcraft-pcm32-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s: Vec<f32> = (0..9_600).map(|i| (i as f32 * 0.01).sin() * 1.5).collect();
        let cases = [
            ("i32.wav", write_wav(&s, 2, 48_000, I32).unwrap(), "pcm_s32le"),
            ("f32.wav", write_wav(&s, 2, 48_000, F32).unwrap(), "pcm_f32le"),
            ("f32-6.wav", write_wav(&s, 6, 48_000, F32).unwrap(), "pcm_f32le"),
            ("i32.aiff", write_aiff(&s, 2, 48_000, I32).unwrap(), "pcm_s32be"),
            ("f32.aifc", write_aiff(&s, 2, 48_000, F32).unwrap(), "pcm_f32be"),
        ];
        for (name, bytes, codec) in cases {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let out = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_name,bits_per_sample", "-of", "default=nw=1"])
                .arg(&path)
                .output();
            let Ok(out) = out else { break };
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains(&format!("codec_name={codec}")) && text.contains("bits_per_sample=32"), "{name}: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reader takes the RF64 sizes: ffprobe reports the duration from `ds64` (skipped without it).
    #[test]
    fn ffprobe_reads_rf64_duration() {
        let dir = std::env::temp_dir().join(format!("filmcraft-rf64-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.wav");
        let frames = 48_000u64 * 60 * 60 * 5; // 5 hours of 24-bit stereo: 5.2 GB
        let mut f = header(PcmContainer::Wav, 2, 48_000, I24, frames).unwrap();
        encode(PcmContainer::Wav, &[0.0; 9_600], I24, &mut f);
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

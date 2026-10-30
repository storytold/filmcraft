//! Radiance HDR (`.hdr`, `.pic`) stills, read from Greg Ward's public RGBE format description
//! ("Real Pixels", Graphics Gems II): a text header (`#?RADIANCE` or `#?RGBE`, `FORMAT=32-bit_rle_rgbe`),
//! a blank line, a resolution line (`-Y h +X w` and the seven other axis orders), then one RGBE
//! pixel (shared exponent) per sample, flat, in the old run-length form or in the "new" per-channel
//! run-length form (scan lines of 8 to 32767 pixels). RGBE is scene-linear light, so the frame keeps
//! 32-bit floats (values above 1.0 survive into effects and colour work). The `EXPOSURE` and
//! `PRIMARIES` header lines are not applied, which is what other readers do too; the XYZE colour
//! format is refused.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Transfer};
use filmcraft_frame::VideoFrame;
use filmcraft_time::FrameRate;

use super::{DEFAULT_STILL_DURATION, StillSource};
use crate::{MediaError, MediaInfo, MediaKind, Result, VideoStreamInfo};

/// Largest picture we decode (each pixel takes 16 bytes as RGBA floats).
const MAX_PIXELS: u64 = 1 << 26;
/// Longest header we read (real ones are a few hundred bytes).
const MAX_HEADER: usize = 64 << 10;

/// The colour of HDR frames: scene-linear light, Rec.709 primaries.
const LINEAR_709: ColorInfo = ColorInfo { transfer: Transfer::Linear, ..ColorInfo::SRGB_FULL };

/// Whether `b` starts like a Radiance picture.
pub fn sniff(b: &[u8]) -> bool {
    b.starts_with(b"#?RADIANCE") || b.starts_with(b"#?RGBE")
}

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: Radiance HDR: {what}"))
}

/// Open a Radiance picture as a still.
pub fn still(name: &str, bytes: &[u8]) -> Result<StillSource> {
    let frame = decode(name, bytes)?;
    let info = MediaInfo {
        name: name.to_string(),
        kind: MediaKind::Still,
        duration: DEFAULT_STILL_DURATION,
        video: Some(VideoStreamInfo {
            width: frame.width,
            height: frame.height,
            frame_rate: FrameRate::FPS_29_97,
            par: (1, 1),
            codec: "HDR".into(),
            pixel_format: "RGB 32-bit float (RGBE)".into(),
            color: LINEAR_709,
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: Vec::new(),
        container: "Image".into(),
        start_timecode: None,
        file_size: Some(bytes.len() as u64),
    };
    Ok(StillSource { info, frame: Arc::new(frame) })
}

/// One resolution-line axis: `(is_x, positive)`.
fn axis(tok: &str) -> Option<(bool, bool)> {
    match tok {
        "+X" => Some((true, true)),
        "-X" => Some((true, false)),
        "+Y" => Some((false, true)),
        "-Y" => Some((false, false)),
        _ => None,
    }
}

/// RGBE to linear floats: `mantissa * 2^(e - 136)`, zero for exponent 0. (Radiance's own reader adds
/// half a mantissa step; ffmpeg and stb_image do not, which also makes 1.0 = (128, 128, 128, 129)
/// read back as exactly 1.0.)
fn rgbe(p: [u8; 4]) -> [f32; 3] {
    if p[3] == 0 {
        return [0.0; 3];
    }
    let scale = 2f32.powi(i32::from(p[3]) - 136);
    [f32::from(p[0]) * scale, f32::from(p[1]) * scale, f32::from(p[2]) * scale]
}

/// Read the pixels of one scan line of `n` pixels into `line`.
fn scanline(name: &str, b: &[u8], pos: &mut usize, line: &mut [[u8; 4]]) -> Result<()> {
    let n = line.len();
    let take = |pos: &mut usize, k: usize| -> Result<&[u8]> {
        let s = b.get(*pos..pos.saturating_add(k)).ok_or_else(|| err(name, "truncated pixel data"))?;
        *pos += k;
        Ok(s)
    };
    let head = b.get(*pos..*pos + 4);
    if (8..=32767).contains(&n)
        && let Some(&[2, 2, hi, lo]) = head
        && hi & 0x80 == 0
    {
        if usize::from(hi) << 8 | usize::from(lo) != n {
            return Err(err(name, "scan line length does not match the picture"));
        }
        *pos += 4;
        for c in 0..4 {
            let mut x = 0;
            while x < n {
                let code = take(pos, 1)?[0];
                if code > 128 {
                    let run = usize::from(code - 128);
                    let v = take(pos, 1)?[0];
                    let dst = line.get_mut(x..x + run).ok_or_else(|| err(name, "run past the end of the scan line"))?;
                    for p in dst {
                        p[c] = v;
                    }
                    x += run;
                } else {
                    let run = usize::from(code);
                    if run == 0 {
                        return Err(err(name, "empty literal run"));
                    }
                    let src = take(pos, run)?;
                    let dst = line.get_mut(x..x + run).ok_or_else(|| err(name, "literal past the end of the scan line"))?;
                    for (p, &v) in dst.iter_mut().zip(src) {
                        p[c] = v;
                    }
                    x += run;
                }
            }
        }
        return Ok(());
    }
    // flat pixels, with the old run-length form: (1, 1, 1, k) repeats the previous pixel k << shift times
    let (mut x, mut shift) = (0usize, 0u32);
    while x < n {
        let s = take(pos, 4)?;
        let p = [s[0], s[1], s[2], s[3]];
        if p[..3] == [1, 1, 1] {
            let count = usize::from(p[3]).checked_shl(shift).unwrap_or(usize::MAX);
            let prev = if x > 0 { line.get(x - 1).copied() } else { None }.ok_or_else(|| err(name, "run with no previous pixel"))?;
            let dst = line.get_mut(x..x.saturating_add(count).min(n)).ok_or_else(|| err(name, "run past the end of the scan line"))?;
            dst.fill(prev);
            x += dst.len();
            shift = shift.saturating_add(8).min(24);
        } else {
            if let Some(d) = line.get_mut(x) {
                *d = p;
            }
            x += 1;
            shift = 0;
        }
    }
    Ok(())
}

/// Decode a Radiance picture to an opaque linear float frame.
pub fn decode(name: &str, b: &[u8]) -> Result<VideoFrame> {
    if !sniff(b) {
        return Err(err(name, "not a Radiance picture"));
    }
    // header: lines up to the first empty one, then the resolution line
    let mut pos = 0;
    let line = |pos: &mut usize| -> Result<String> {
        let rest = b.get(*pos..).ok_or_else(|| err(name, "truncated header"))?;
        let end = rest.iter().take(MAX_HEADER).position(|&c| c == b'\n').ok_or_else(|| err(name, "truncated header"))?;
        let s = String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).trim_end_matches('\r').to_string();
        *pos += end + 1;
        Ok(s)
    };
    loop {
        let l = line(&mut pos)?;
        if l.is_empty() {
            break;
        }
        if let Some(f) = l.strip_prefix("FORMAT=") {
            match f.trim() {
                "32-bit_rle_rgbe" => {}
                "32-bit_rle_xyze" => return Err(err(name, "the XYZE colour format is not supported")),
                other => return Err(err(name, &format!("unknown format {other}"))),
            }
        }
        if pos > MAX_HEADER {
            return Err(err(name, "header too long"));
        }
    }
    let res = line(&mut pos)?;
    let t: Vec<&str> = res.split_whitespace().collect();
    let &[a1, n1, a2, n2] = t.as_slice().first_chunk::<4>().ok_or_else(|| err(name, "bad resolution line"))?;
    let ((x1, p1), (x2, p2)) = (axis(a1).ok_or_else(|| err(name, "bad resolution line"))?, axis(a2).ok_or_else(|| err(name, "bad resolution line"))?);
    if x1 == x2 {
        return Err(err(name, "resolution line names one axis twice"));
    }
    let n1: usize = n1.parse().map_err(|_| err(name, "bad resolution line"))?;
    let n2: usize = n2.parse().map_err(|_| err(name, "bad resolution line"))?;
    let (w, h) = if x1 { (n1, n2) } else { (n2, n1) };
    if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return Err(err(name, "picture size out of range"));
    }
    // a scan line takes at least a few bits of file, so a header cannot ask for more than the data has
    if (b.len().saturating_sub(pos) as u64).saturating_mul(4096) < (w as u64).saturating_mul(h as u64) {
        return Err(err(name, "truncated pixel data"));
    }
    // Validate every encoded scanline before allocating the complete float image. A short HDR
    // header can otherwise request up to a GiB of zero-initialized pixels and only then fail on
    // its first missing or malformed scanline.
    let pixel_data = pos;
    let mut scan = vec![[0u8; 4]; n2];
    for _ in 0..n1 {
        scanline(name, b, &mut pos, &mut scan)?;
    }

    let mut px = vec![1f32; w * h * 4];
    pos = pixel_data;
    for i in 0..n1 {
        scanline(name, b, &mut pos, &mut scan)?;
        for (j, &p) in scan.iter().enumerate() {
            // the first axis counts scan lines, the second runs along one
            let (x, y) = if x1 {
                (if p1 { i } else { w - 1 - i }, if p2 { h - 1 - j } else { j })
            } else {
                (if p2 { j } else { w - 1 - j }, if p1 { h - 1 - i } else { i })
            };
            let [r, g, bl] = rgbe(p);
            if let Some(o) = px.get_mut((y * w + x) * 4..(y * w + x) * 4 + 3) {
                o.copy_from_slice(&[r, g, bl]);
            }
        }
    }
    let mut frame = VideoFrame::rgba_f32(w as u32, h as u32, px);
    frame.color = LINEAR_709;
    Ok(frame)
}

#[cfg(test)]
mod tests;

//! Multi-frame visual timeline sampling and image composition for agent verification.
//!
//! Stitches multiple rendered video frames into a single composite PNG image (grid or linear strip)
//! with timestamp and edit labels overlaid on each thumbnail, allowing multimodal agents to inspect
//! pacing, continuity, and cut points in a single token-efficient request.

use image::{ImageFormat, Rgba, RgbaImage, imageops};
use std::io::Cursor;

use crate::AutomationError;

/// One rendered frame sampled from the timeline.
#[derive(Debug, Clone)]
pub struct SampledFrame {
    pub seconds: f64,
    pub label: Option<String>,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Layout format for the composite contact sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SampleLayout {
    /// 2D tiled grid (e.g. 2x2, 3x2).
    Grid,
    /// Single horizontal filmstrip (ideal for A/B cut point continuity checks).
    Strip,
}

impl Default for SampleLayout {
    fn default() -> Self {
        Self::Grid
    }
}

/// Format seconds as standard MM:SS.CC or HH:MM:SS.CC timecode text.
pub fn format_timestamp(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00.00".into();
    }
    let total_centis = (seconds * 100.0).round() as u64;
    let centis = total_centis % 100;
    let total_secs = total_centis / 100;
    let s = total_secs % 60;
    let m = (total_secs / 60) % 60;
    let h = total_secs / 3600;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}.{centis:02}")
    } else {
        format!("{m:02}:{s:02}.{centis:02}")
    }
}

/// Calculate grid dimensions (columns, rows) for `count` items.
pub fn grid_dimensions(count: usize, layout: SampleLayout) -> (usize, usize) {
    if count <= 1 {
        return (count.max(1), 1);
    }
    if layout == SampleLayout::Strip || count <= 3 {
        return (count, 1);
    }
    match count {
        4 => (2, 2),
        5 | 6 => (3, 2),
        7..=9 => (3, 3),
        10..=12 => (4, 3),
        13..=16 => (4, 4),
        _ => {
            let cols = (count as f64).sqrt().ceil() as usize;
            let cols = cols.max(1);
            let rows = count.div_ceil(cols);
            (cols, rows)
        }
    }
}

/// 5x7 minimal bitmap font glyph rows (7 rows, top 5 bits per row).
fn glyph_rows(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        'A' | 'a' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' | 'b' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' | 'c' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' | 'd' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' | 'e' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' | 'f' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' | 'g' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' | 'h' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' | 'i' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'L' | 'l' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' | 'm' => [0x11, 0x1B, 0x15, 0x11, 0x11, 0x11, 0x11],
        'N' | 'n' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' | 'o' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' | 'p' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' | 'r' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' | 's' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' | 't' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' | 'u' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' | 'v' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        _ => [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F],
    }
}

/// Draw a single bitmap character with scaling.
fn draw_char(img: &mut RgbaImage, x: u32, y: u32, c: char, scale: u32, fg: Rgba<u8>) {
    let glyph = glyph_rows(c);
    let (iw, ih) = img.dimensions();
    for (row_idx, &row) in glyph.iter().enumerate() {
        for col_idx in 0..5 {
            if (row & (1 << (4 - col_idx))) != 0 {
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = x.saturating_add((col_idx as u32).saturating_mul(scale)).saturating_add(sx);
                        let py = y.saturating_add((row_idx as u32).saturating_mul(scale)).saturating_add(sy);
                        if px < iw && py < ih {
                            img.put_pixel(px, py, fg);
                        }
                    }
                }
            }
        }
    }
}

/// Draw a dark semi-translucent banner with white text at `(x, y)`.
pub fn draw_text_banner(img: &mut RgbaImage, x: u32, y: u32, text: &str, scale: u32) {
    if text.is_empty() {
        return;
    }
    let scale = scale.max(1);
    let char_width = 5 * scale;
    let char_spacing = 1 * scale;
    let char_height = 7 * scale;
    let pad_x = 4 * scale;
    let pad_y = 3 * scale;

    let total_width = pad_x.saturating_mul(2).saturating_add((text.len() as u32).saturating_mul(char_width.saturating_add(char_spacing)));
    let total_height = pad_y.saturating_mul(2).saturating_add(char_height);

    let (iw, ih) = img.dimensions();
    let bx_max = x.saturating_add(total_width).min(iw);
    let by_max = y.saturating_add(total_height).min(ih);

    // Alpha blend background rectangle: rgba(0, 0, 0, 190)
    for py in y..by_max {
        for px in x..bx_max {
            let p = img.get_pixel(px, py);
            let r = (p[0] as u32 * 65) / 255;
            let g = (p[1] as u32 * 65) / 255;
            let b = (p[2] as u32 * 65) / 255;
            img.put_pixel(px, py, Rgba([r as u8, g as u8, b as u8, 255]));
        }
    }

    // Render characters
    let mut cx = x.saturating_add(pad_x);
    let cy = y.saturating_add(pad_y);
    for c in text.chars() {
        draw_char(img, cx, cy, c, scale, Rgba([255, 255, 255, 255]));
        cx = cx.saturating_add(char_width).saturating_add(char_spacing);
    }
}

/// Stitch multiple sampled frames into a single composite PNG image.
pub fn stitch(frames: &[SampledFrame], layout: SampleLayout, max_side: u32) -> Result<Vec<u8>, AutomationError> {
    if frames.is_empty() {
        return Err(AutomationError::BadRequest("no frames to sample".into()));
    }

    // Handle single frame directly with banner
    if frames.len() == 1 {
        let f = &frames[0];
        let mut img = RgbaImage::from_raw(f.width, f.height, f.rgba.clone())
            .ok_or_else(|| AutomationError::Other("frame dimensions do not match RGBA buffer".into()))?;

        let label = f.label.clone().unwrap_or_else(|| format_timestamp(f.seconds));
        let scale = if f.width >= 640 { 2 } else { 1 };
        let banner_h = 13 * scale;
        let by = f.height.saturating_sub(banner_h).saturating_sub(8);
        draw_text_banner(&mut img, 8, by, &label, scale);

        return encode_png(img, max_side);
    }

    let (cols, rows) = grid_dimensions(frames.len(), layout);
    let cols_u32 = u32::try_from(cols).unwrap_or(1);
    let rows_u32 = u32::try_from(rows).unwrap_or(1);

    // Common cell dimensions based on max observed frame dimensions
    let cell_w = frames.iter().map(|f| f.width).max().unwrap_or(320).max(1);
    let cell_h = frames.iter().map(|f| f.height).max().unwrap_or(180).max(1);
    let border: u32 = 4;

    let canvas_w = cols_u32
        .checked_mul(cell_w)
        .and_then(|w| w.checked_add((cols_u32 + 1) * border))
        .ok_or_else(|| AutomationError::Other("canvas width overflows".into()))?;
    let canvas_h = rows_u32
        .checked_mul(cell_h)
        .and_then(|h| h.checked_add((rows_u32 + 1) * border))
        .ok_or_else(|| AutomationError::Other("canvas height overflows".into()))?;

    // Create dark slate background canvas: #18181b
    let mut canvas = RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([24, 24, 27, 255]));

    let text_scale = if cell_w >= 480 { 2 } else { 1 };
    let banner_h = 13 * text_scale;

    for (idx, f) in frames.iter().enumerate() {
        let col = (idx % cols) as u32;
        let row = (idx / cols) as u32;

        let ox = border + col * (cell_w + border);
        let oy = border + row * (cell_h + border);

        let tile_img = match RgbaImage::from_raw(f.width, f.height, f.rgba.clone()) {
            Some(img) if f.width == cell_w && f.height == cell_h => img,
            Some(img) => imageops::resize(&img, cell_w, cell_h, imageops::FilterType::Triangle),
            None => continue,
        };

        imageops::overlay(&mut canvas, &tile_img, ox as i64, oy as i64);

        let label = f.label.clone().unwrap_or_else(|| format_timestamp(f.seconds));
        let by = oy + cell_h.saturating_sub(banner_h).saturating_sub(6);
        draw_text_banner(&mut canvas, ox + 6, by, &label, text_scale);
    }

    encode_png(canvas, max_side)
}

fn encode_png(mut img: RgbaImage, max_side: u32) -> Result<Vec<u8>, AutomationError> {
    let (w, h) = img.dimensions();
    if max_side > 0 && w.max(h) > max_side {
        let scale = max_side as f32 / w.max(h) as f32;
        let target_w = ((w as f32 * scale) as u32).max(1);
        let target_h = ((h as f32 * scale) as u32).max(1);
        img = imageops::resize(&img, target_w, target_h, imageops::FilterType::Triangle);
    }

    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).map_err(|e| AutomationError::Other(e.to_string()))?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_frame(sec: f64, label: Option<&str>, w: u32, h: u32, color: [u8; 4]) -> SampledFrame {
        let rgba = vec![color[0], color[1], color[2], color[3]].repeat((w * h) as usize);
        SampledFrame { seconds: sec, label: label.map(String::from), width: w, height: h, rgba }
    }

    #[test]
    fn format_timestamp_values() {
        assert_eq!(format_timestamp(0.0), "00:00.00");
        assert_eq!(format_timestamp(1.5), "00:01.50");
        assert_eq!(format_timestamp(65.25), "01:05.25");
        assert_eq!(format_timestamp(3665.0), "01:01:05.00");
        assert_eq!(format_timestamp(-1.0), "00:00.00");
        assert_eq!(format_timestamp(f64::NAN), "00:00.00");
    }

    #[test]
    fn grid_dimension_calculation() {
        assert_eq!(grid_dimensions(1, SampleLayout::Grid), (1, 1));
        assert_eq!(grid_dimensions(4, SampleLayout::Grid), (2, 2));
        assert_eq!(grid_dimensions(6, SampleLayout::Grid), (3, 2));
        assert_eq!(grid_dimensions(4, SampleLayout::Strip), (4, 1));
    }

    #[test]
    fn empty_frames_returns_error() {
        let r = stitch(&[], SampleLayout::Grid, 800);
        assert!(matches!(r, Err(AutomationError::BadRequest(_))));
    }

    #[test]
    fn single_frame_stitch() {
        let f = dummy_frame(2.5, Some("00:02.50"), 160, 90, [100, 150, 200, 255]);
        let png = stitch(&[f], SampleLayout::Grid, 0).unwrap();
        assert!(!png.is_empty());
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn multi_frame_grid_stitch() {
        let frames = vec![
            dummy_frame(1.0, Some("Shot 1"), 160, 90, [200, 50, 50, 255]),
            dummy_frame(2.0, Some("Shot 2"), 160, 90, [50, 200, 50, 255]),
            dummy_frame(3.0, Some("Shot 3"), 160, 90, [50, 50, 200, 255]),
            dummy_frame(4.0, Some("Shot 4"), 160, 90, [200, 200, 50, 255]),
        ];
        let png = stitch(&frames, SampleLayout::Grid, 300).unwrap();
        assert!(!png.is_empty());
        let decoded = image::load_from_memory(&png).unwrap();
        assert!(decoded.width() <= 300 && decoded.height() <= 300);
    }
}

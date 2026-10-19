//! Model-specific resampling and trimap helpers. Input validation precedes hot loops.

pub fn bilinear(data: &[f32], width: usize, height: usize, out_width: usize, out_height: usize) -> Result<Vec<f32>, String> {
    if width == 0
        || height == 0
        || out_width == 0
        || out_height == 0
        || width.checked_mul(height) != Some(data.len())
        || out_width.checked_mul(out_height).is_none_or(|n| n > 4_194_304)
    {
        return Err("Invalid resize dimensions".into());
    }
    let mut result = vec![0.0; out_width * out_height];
    for y in 0..out_height {
        let sy = ((y as f32 + 0.5) * height as f32 / out_height as f32 - 0.5).max(0.0);
        let y0 = (sy.floor() as usize).min(height - 1);
        let y1 = (y0 + 1).min(height - 1);
        let dy = sy - y0 as f32;
        for x in 0..out_width {
            let sx = ((x as f32 + 0.5) * width as f32 / out_width as f32 - 0.5).max(0.0);
            let x0 = (sx.floor() as usize).min(width - 1);
            let x1 = (x0 + 1).min(width - 1);
            let dx = sx - x0 as f32;
            let a = data[y0 * width + x0] * (1.0 - dx) + data[y0 * width + x1] * dx;
            let b = data[y1 * width + x0] * (1.0 - dx) + data[y1 * width + x1] * dx;
            result[y * out_width + x] = a * (1.0 - dy) + b * dy;
        }
    }
    Ok(result)
}

pub fn normalized_image(rgba: &[u8], width: usize, height: usize) -> Result<Vec<f32>, String> {
    let pixels = width.checked_mul(height).ok_or("image dimensions overflow")?;
    if width == 0 || height == 0 || pixels.checked_mul(4) != Some(rgba.len()) || pixels > 4_194_304 {
        return Err("Invalid RGBA image".into());
    }
    let mut output = Vec::with_capacity(3 * 1024 * 1024);
    for (channel, (mean, std)) in [(0.485, 0.229), (0.456, 0.224), (0.406, 0.225)].into_iter().enumerate() {
        let plane: Vec<_> = rgba.chunks_exact(4).map(|p| p[channel] as f32 / 255.0).collect();
        output.extend(bilinear(&plane, width, height, 1024, 1024)?.into_iter().map(|x| (x - mean) / std));
    }
    Ok(output)
}

pub fn trimap(mask: &[bool], width: usize, height: usize, radius: usize) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(mask.len()) || mask.len() > 4_194_304 || !(1..=32).contains(&radius) {
        return Err("Invalid trimap dimensions/band".into());
    }
    let stride = width + 1;
    let mut sums = vec![0u32; stride * (height + 1)];
    for y in 0..height {
        let mut row = 0u32;
        for x in 0..width {
            row += u32::from(mask[y * width + x]);
            sums[(y + 1) * stride + x + 1] = sums[y * stride + x + 1] + row;
        }
    }
    let full = ((radius * 2 + 1) * (radius * 2 + 1)) as i64;
    let mut output = vec![128u8; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let x0 = x.saturating_sub(radius);
            let y0 = y.saturating_sub(radius);
            let x1 = (x + radius + 1).min(width);
            let y1 = (y + radius + 1).min(height);
            let count = sums[y1 * stride + x1] as i64 + sums[y0 * stride + x0] as i64 - sums[y1 * stride + x0] as i64 - sums[y0 * stride + x1] as i64;
            output[y * width + x] = if count == 0 {
                0
            } else if count == full {
                255
            } else {
                128
            };
        }
    }
    Ok(output)
}

/// ViTMatte's 14² absolute positions, with the class token removed, resized using
/// PyTorch's half-pixel bicubic convention (coefficient -0.75, border replication).
pub fn absolute_position(base: &[f32], height: usize, width: usize) -> Result<Vec<f32>, String> {
    if base.len() != 197 * 384 || height == 0 || width == 0 || height > 32 || width > 32 {
        return Err("Invalid matte position dimensions".into());
    }
    fn coefficient(distance: f32) -> f32 {
        let x = distance.abs();
        if x <= 1.0 {
            ((1.25 * x - 2.25) * x) * x + 1.0
        } else if x < 2.0 {
            ((-0.75 * x + 3.75) * x - 6.0) * x + 3.0
        } else {
            0.0
        }
    }
    let mut output = vec![0.0; height * width * 384];
    for y in 0..height {
        let sy = (y as f32 + 0.5) * 14.0 / height as f32 - 0.5;
        let iy = sy.floor() as i32;
        for x in 0..width {
            let sx = (x as f32 + 0.5) * 14.0 / width as f32 - 0.5;
            let ix = sx.floor() as i32;
            for channel in 0..384 {
                let mut value = 0.0;
                for dy in -1..=2 {
                    let source_y = (iy + dy).clamp(0, 13) as usize;
                    let mut row = 0.0;
                    for dx in -1..=2 {
                        let source_x = (ix + dx).clamp(0, 13) as usize;
                        row += base[((source_y * 14 + source_x) + 1) * 384 + channel] * coefficient(sx - (ix + dx) as f32);
                    }
                    value += row * coefficient(sy - (iy + dy) as f32);
                }
                output[(y * width + x) * 384 + channel] = value;
            }
        }
    }
    Ok(output)
}

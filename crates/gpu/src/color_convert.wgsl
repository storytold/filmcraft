// Color space and format conversion compute shaders.
// Converts RGBA textures to NV12, YUV 4:2:2 10-bit, P010 10-bit, and packed RGB8.

struct ConvertConfig {
    width: u32,
    height: u32,
    matrix_mode: u32, // 0 = BT.709, 1 = BT.2020
    range_mode: u32,  // 0 = Limited, 1 = Full
}

@group(0) @binding(0) var<uniform> cfg: ConvertConfig;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> dst_buf: array<u32>;

// BT.709 coefficients: Kr = 0.2126, Kb = 0.0722, Kg = 0.7152
// BT.2020 coefficients: Kr = 0.2627, Kb = 0.0593, Kg = 0.6780

fn get_luma(r: f32, g: f32, b: f32) -> f32 {
    if cfg.matrix_mode == 1u {
        return 0.2627 * r + 0.6780 * g + 0.0593 * b;
    }
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

fn get_chroma_u(r: f32, g: f32, b: f32, y: f32) -> f32 {
    if cfg.matrix_mode == 1u {
        return (b - y) / 1.8814;
    }
    return (b - y) / 1.8556;
}

fn get_chroma_v(r: f32, g: f32, b: f32, y: f32) -> f32 {
    if cfg.matrix_mode == 1u {
        return (r - y) / 1.4746;
    }
    return (r - y) / 1.5748;
}

fn quant_y8(y: f32) -> u32 {
    if cfg.range_mode == 1u {
        return clamp(u32(round(255.0 * y)), 0u, 255u);
    }
    return clamp(u32(round(16.0 + 219.0 * y)), 1u, 254u);
}

fn quant_uv8(c: f32) -> u32 {
    if cfg.range_mode == 1u {
        return clamp(u32(round(128.0 + 255.0 * c)), 0u, 255u);
    }
    return clamp(u32(round(128.0 + 224.0 * c)), 1u, 254u);
}

fn quant_y10(y: f32) -> u32 {
    if cfg.range_mode == 1u {
        return clamp(u32(round(1023.0 * y)), 0u, 1023u);
    }
    return clamp(u32(round(64.0 + 876.0 * y)), 4u, 1019u);
}

fn quant_uv10(c: f32) -> u32 {
    if cfg.range_mode == 1u {
        return clamp(u32(round(512.0 + 1023.0 * c)), 0u, 1023u);
    }
    return clamp(u32(round(512.0 + 896.0 * c)), 4u, 1019u);
}

// Convert RGBA to NV12 (8-bit Y plane followed by interleaved UV plane).
// Each thread processes a 4x2 pixel block:
// - Row 0 (4 Y pixels) -> 1 u32 store
// - Row 1 (4 Y pixels) -> 1 u32 store
// - 2 UV pairs (4 bytes) -> 1 u32 store
@compute @workgroup_size(16, 16)
fn cs_rgba_to_nv12(@builtin(global_invocation_id) gid: vec3<u32>) {
    let bx = gid.x * 4u;
    let by = gid.y * 2u;
    let w = cfg.width;
    let h = cfg.height;

    if bx >= w || by >= h {
        return;
    }

    let iw = i32(w);
    let ih = i32(h);

    // Read 4 pixels of row by
    let p00 = textureLoad(src_tex, vec2<i32>(i32(bx), i32(by)), 0);
    let p10 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 1u), iw - 1), i32(by)), 0);
    let p20 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 2u), iw - 1), i32(by)), 0);
    let p30 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 3u), iw - 1), i32(by)), 0);

    // Read 4 pixels of row by + 1
    let by1 = min(i32(by + 1u), ih - 1);
    let p01 = textureLoad(src_tex, vec2<i32>(i32(bx), by1), 0);
    let p11 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 1u), iw - 1), by1), 0);
    let p21 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 2u), iw - 1), by1), 0);
    let p31 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 3u), iw - 1), by1), 0);

    let y00 = get_luma(p00.r, p00.g, p00.b);
    let y10 = get_luma(p10.r, p10.g, p10.b);
    let y20 = get_luma(p20.r, p20.g, p20.b);
    let y30 = get_luma(p30.r, p30.g, p30.b);

    let y01 = get_luma(p01.r, p01.g, p01.b);
    let y11 = get_luma(p11.r, p11.g, p11.b);
    let y21 = get_luma(p21.r, p21.g, p21.b);
    let y31 = get_luma(p31.r, p31.g, p31.b);

    // Pack Y row 0
    let cy00 = quant_y8(y00);
    let cy10 = quant_y8(y10);
    let cy20 = quant_y8(y20);
    let cy30 = quant_y8(y30);
    let y_word0 = cy00 | (cy10 << 8u) | (cy20 << 16u) | (cy30 << 24u);

    // Pack Y row 1
    let cy01 = quant_y8(y01);
    let cy11 = quant_y8(y11);
    let cy21 = quant_y8(y21);
    let cy31 = quant_y8(y31);
    let y_word1 = cy01 | (cy11 << 8u) | (cy21 << 16u) | (cy31 << 24u);

    // Write Y rows
    let y_idx0 = (by * w + bx) / 4u;
    dst_buf[y_idx0] = y_word0;

    if by + 1u < h {
        let y_idx1 = ((by + 1u) * w + bx) / 4u;
        dst_buf[y_idx1] = y_word1;
    }

    // Chroma: block 0 is pixels (00, 10, 01, 11); block 1 is (20, 30, 21, 31)
    let u0 = (get_chroma_u(p00.r, p00.g, p00.b, y00) + get_chroma_u(p10.r, p10.g, p10.b, y10) +
              get_chroma_u(p01.r, p01.g, p01.b, y01) + get_chroma_u(p11.r, p11.g, p11.b, y11)) * 0.25;
    let v0 = (get_chroma_v(p00.r, p00.g, p00.b, y00) + get_chroma_v(p10.r, p10.g, p10.b, y10) +
              get_chroma_v(p01.r, p01.g, p01.b, y01) + get_chroma_v(p11.r, p11.g, p11.b, y11)) * 0.25;

    let u1 = (get_chroma_u(p20.r, p20.g, p20.b, y20) + get_chroma_u(p30.r, p30.g, p30.b, y30) +
              get_chroma_u(p21.r, p21.g, p21.b, y21) + get_chroma_u(p31.r, p31.g, p31.b, y31)) * 0.25;
    let v1 = (get_chroma_v(p20.r, p20.g, p20.b, y20) + get_chroma_v(p30.r, p30.g, p30.b, y30) +
              get_chroma_v(p21.r, p21.g, p21.b, y21) + get_chroma_v(p31.r, p31.g, p31.b, y31)) * 0.25;

    let cu0 = quant_uv8(u0);
    let cv0 = quant_uv8(v0);
    let cu1 = quant_uv8(u1);
    let cv1 = quant_uv8(v1);

    let uv_word = cu0 | (cv0 << 8u) | (cu1 << 16u) | (cv1 << 24u);

    // UV plane starts at byte offset w * h
    let uv_offset_bytes = w * h + gid.y * w + bx;
    let uv_idx = uv_offset_bytes / 4u;
    dst_buf[uv_idx] = uv_word;
}

// Convert RGBA to 10-bit YUV 4:2:2 planar (for ProRes 422 and DNxHR).
// Each thread processes 4 horizontal pixels on row y:
// - Y plane: 4 x u16 -> 2 u32 stores
// - Cb plane: 2 x u16 -> 1 u32 store
// - Cr plane: 2 x u16 -> 1 u32 store
@compute @workgroup_size(16, 16)
fn cs_rgba_to_yuv422_10(@builtin(global_invocation_id) gid: vec3<u32>) {
    let bx = gid.x * 4u;
    let y = gid.y;
    let w = cfg.width;
    let h = cfg.height;

    if bx >= w || y >= h {
        return;
    }

    let iw = i32(w);
    let iy = i32(y);

    let p0 = textureLoad(src_tex, vec2<i32>(i32(bx), iy), 0);
    let p1 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 1u), iw - 1), iy), 0);
    let p2 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 2u), iw - 1), iy), 0);
    let p3 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 3u), iw - 1), iy), 0);

    let y0 = get_luma(p0.r, p0.g, p0.b);
    let y1 = get_luma(p1.r, p1.g, p1.b);
    let y2 = get_luma(p2.r, p2.g, p2.b);
    let y3 = get_luma(p3.r, p3.g, p3.b);

    let cy0 = quant_y10(y0);
    let cy1 = quant_y10(y1);
    let cy2 = quant_y10(y2);
    let cy3 = quant_y10(y3);

    let y_word0 = cy0 | (cy1 << 16u);
    let y_word1 = cy2 | (cy3 << 16u);

    // Y plane is w * h * 2 bytes = (w * h / 2) u32 words
    let y_idx = (y * w + bx) / 2u;
    dst_buf[y_idx] = y_word0;
    dst_buf[y_idx + 1u] = y_word1;

    // Chroma pairs: (p0, p1) and (p2, p3)
    let u0 = (get_chroma_u(p0.r, p0.g, p0.b, y0) + get_chroma_u(p1.r, p1.g, p1.b, y1)) * 0.5;
    let v0 = (get_chroma_v(p0.r, p0.g, p0.b, y0) + get_chroma_v(p1.r, p1.g, p1.b, y1)) * 0.5;

    let u1 = (get_chroma_u(p2.r, p2.g, p2.b, y2) + get_chroma_u(p3.r, p3.g, p3.b, y3)) * 0.5;
    let v1 = (get_chroma_v(p2.r, p2.g, p2.b, y2) + get_chroma_v(p3.r, p3.g, p3.b, y3)) * 0.5;

    let cu0 = quant_uv10(u0);
    let cu1 = quant_uv10(u1);
    let cv0 = quant_uv10(v0);
    let cv1 = quant_uv10(v1);

    let u_word = cu0 | (cu1 << 16u);
    let v_word = cv0 | (cv1 << 16u);

    let cw = (w + 1u) / 2u;
    let cb_start_words = (w * h) / 2u;
    let cr_start_words = cb_start_words + (cw * h) / 2u;

    let c_idx = (y * cw + (bx / 2u)) / 2u;
    dst_buf[cb_start_words + c_idx] = u_word;
    dst_buf[cr_start_words + c_idx] = v_word;
}

// Convert RGBA to packed RGB8 (no alpha padding, 3 bytes per pixel).
// Each thread processes 4 pixels (12 bytes = 3 u32 words).
@compute @workgroup_size(16, 16)
fn cs_rgba_to_rgb8(@builtin(global_invocation_id) gid: vec3<u32>) {
    let bx = gid.x * 4u;
    let y = gid.y;
    let w = cfg.width;
    let h = cfg.height;

    if bx >= w || y >= h {
        return;
    }

    let iw = i32(w);
    let iy = i32(y);

    let p0 = textureLoad(src_tex, vec2<i32>(i32(bx), iy), 0);
    let p1 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 1u), iw - 1), iy), 0);
    let p2 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 2u), iw - 1), iy), 0);
    let p3 = textureLoad(src_tex, vec2<i32>(min(i32(bx + 3u), iw - 1), iy), 0);

    let r0 = clamp(u32(round(p0.r * 255.0)), 0u, 255u);
    let g0 = clamp(u32(round(p0.g * 255.0)), 0u, 255u);
    let b0 = clamp(u32(round(p0.b * 255.0)), 0u, 255u);

    let r1 = clamp(u32(round(p1.r * 255.0)), 0u, 255u);
    let g1 = clamp(u32(round(p1.g * 255.0)), 0u, 255u);
    let b1 = clamp(u32(round(p1.b * 255.0)), 0u, 255u);

    let r2 = clamp(u32(round(p2.r * 255.0)), 0u, 255u);
    let g2 = clamp(u32(round(p2.g * 255.0)), 0u, 255u);
    let b2 = clamp(u32(round(p2.b * 255.0)), 0u, 255u);

    let r3 = clamp(u32(round(p3.r * 255.0)), 0u, 255u);
    let g3 = clamp(u32(round(p3.g * 255.0)), 0u, 255u);
    let b3 = clamp(u32(round(p3.b * 255.0)), 0u, 255u);

    let w0 = r0 | (g0 << 8u) | (b0 << 16u) | (r1 << 24u);
    let w1 = g1 | (b1 << 8u) | (r2 << 16u) | (g2 << 24u);
    let w2 = b2 | (r3 << 8u) | (g3 << 16u) | (b3 << 24u);

    let out_byte_idx = (y * w + bx) * 3u;
    let word_idx = out_byte_idx / 4u;
    dst_buf[word_idx] = w0;
    dst_buf[word_idx + 1u] = w1;
    dst_buf[word_idx + 2u] = w2;
}

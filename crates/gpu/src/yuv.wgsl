// Export frames as 8-bit BT.709 limited 4:2:0 (yuv.rs). One invocation per 8x2 pixels: two Y
// words per row (8 bytes) and one U and one V word (4 chroma samples), so no two invocations
// write the same word. Padding bytes (x >= w) are 0; rows y >= h are not written.

struct Params {
    w: u32,
    h: u32,
    y_stride: u32,
    c_stride: u32,
    u_off: u32,
    v_off: u32,
    groups_x: u32,
    groups_y: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var accum: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> lut: array<u32>;
@group(0) @binding(3) var<storage, read_write> out_words: array<u32>;

// filmcraft_color::linear_to_srgb_u8: table index clamp(v, 0, 1) * 4095 + 0.5
fn srgb8(v: f32) -> u32 {
    let i = u32(clamp(v, 0.0, 1.0) * 4095.0 + 0.5);
    return lut[min(i, 4095u)];
}

// Rust's f32::round (half away from zero) for these positive values, then clamp(1, 254)
fn code(v: f32) -> u32 {
    return u32(clamp(floor(v + 0.5), 1.0, 254.0));
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.groups_x || id.y >= p.groups_y) {
        return;
    }
    let x0 = id.x * 8u;
    let y0 = id.y * 2u;
    var ywords = array<u32, 4>(0u, 0u, 0u, 0u); // row 0 lo, row 0 hi, row 1 lo, row 1 hi
    var uword = 0u;
    var vword = 0u;
    for (var c = 0u; c < 4u; c++) {
        var us = 0.0;
        var vs = 0.0;
        var cnt = 0.0;
        for (var dy = 0u; dy < 2u; dy++) {
            let yy = y0 + dy;
            if (yy >= p.h) {
                continue;
            }
            for (var dx = 0u; dx < 2u; dx++) {
                let xx = x0 + c * 2u + dx;
                if (xx >= p.w) {
                    continue;
                }
                let s = textureLoad(accum, vec2<i32>(i32(xx), i32(yy)), 0);
                let r = f32(srgb8(s.r)) / 255.0;
                let g = f32(srgb8(s.g)) / 255.0;
                let b = f32(srgb8(s.b)) / 255.0;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let k = c * 2u + dx;
                ywords[dy * 2u + k / 4u] |= code(16.0 + 219.0 * luma) << (8u * (k % 4u));
                us += (b - luma) / 1.8556;
                vs += (r - luma) / 1.5748;
                cnt += 1.0;
            }
        }
        if (cnt > 0.0) {
            uword |= code(128.0 + 224.0 * us / cnt) << (8u * c);
            vword |= code(128.0 + 224.0 * vs / cnt) << (8u * c);
        }
    }
    let row_words = p.y_stride / 4u;
    let base = y0 * row_words + id.x * 2u;
    out_words[base] = ywords[0];
    out_words[base + 1u] = ywords[1];
    if (y0 + 1u < p.h) {
        out_words[base + row_words] = ywords[2];
        out_words[base + row_words + 1u] = ywords[3];
    }
    let crow = id.y * (p.c_stride / 4u) + id.x;
    out_words[p.u_off / 4u + crow] = uword;
    out_words[p.v_off / 4u + crow] = vword;
}

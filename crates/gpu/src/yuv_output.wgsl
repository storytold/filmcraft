struct Params {
    width: u32,
    height: u32,
    coded_width: u32,
    coded_height: u32,
    y_stride: u32,
    uv_stride: u32,
    uv_offset: u32,
    groups_x: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var accum: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> srgb_lut: array<u32>;
@group(0) @binding(3) var<storage, read_write> out_words: array<u32>;

fn srgb8(v: f32) -> u32 {
    let i = u32(clamp(v, 0.0, 1.0) * 4095.0 + 0.5);
    return srgb_lut[min(i, 4095u)];
}

fn code(v: f32) -> u32 {
    return u32(clamp(floor(v + 0.5), 1.0, 254.0));
}

// Each invocation writes two 8-pixel luma rows and sixteen interleaved chroma bytes.
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.groups_x || id.y >= (p.coded_height + 1u) / 2u) {
        return;
    }
    let x0 = id.x * 8u;
    let y0 = id.y * 2u;
    var ywords = array<u32, 4>(0u, 0u, 0u, 0u);
    var uvwords = array<u32, 4>(0u, 0u, 0u, 0u);
    for (var c = 0u; c < 4u; c++) {
        var us = 0.0;
        var vs = 0.0;
        var count = 0.0;
        for (var dy = 0u; dy < 2u; dy++) {
            let y = y0 + dy;
            if (y >= p.coded_height) { continue; }
            for (var dx = 0u; dx < 2u; dx++) {
                let x = min(x0 + c * 2u + dx, p.width - 1u);
                let sample_y = min(y, p.height - 1u);
                let s = textureLoad(accum, vec2<i32>(i32(x), i32(sample_y)), 0);
                let r = f32(srgb8(s.r)) / 255.0;
                let g = f32(srgb8(s.g)) / 255.0;
                let b = f32(srgb8(s.b)) / 255.0;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let k = c * 2u + dx;
                ywords[dy * 2u + k / 4u] |= code(16.0 + 219.0 * luma) << (8u * (k % 4u));
                us += (b - luma) / 1.8556;
                vs += (r - luma) / 1.5748;
                count += 1.0;
            }
        }
        if (count > 0.0) {
            let u = code(128.0 + 224.0 * us / count);
            let v = code(128.0 + 224.0 * vs / count);
            let shift = 16u * (c % 2u);
            uvwords[c / 2u] |= (u | (v << 8u)) << shift;
        }
    }
    let y_words_per_row = p.y_stride / 4u;
    let y_base = y0 * y_words_per_row + id.x * 2u;
    out_words[y_base] = ywords[0];
    out_words[y_base + 1u] = ywords[1];
    if (y0 + 1u < p.coded_height) {
        out_words[y_base + y_words_per_row] = ywords[2];
        out_words[y_base + y_words_per_row + 1u] = ywords[3];
    }
    let uv_words_per_row = p.uv_stride / 4u;
    let uv_base = p.uv_offset / 4u + id.y * uv_words_per_row + id.x * 2u;
    out_words[uv_base] = uvwords[0];
    out_words[uv_base + 1u] = uvwords[1];
}

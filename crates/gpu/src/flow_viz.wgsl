// Optical-flow overlay frames: the GPU counterpart of `filmcraft_render::flow::render_flow_viz`
// followed by `image_to_encoded_rgb_f32` (same formulas, step for step). Writes the encoded
// R'G'B' floats, 3 per pixel, ready for the ProRes encoder.

struct Params {
    out_w: u32,
    out_h: u32,
    grid_w: u32,
    grid_h: u32,
    // the flow field's own frame size (its vectors are in these pixels)
    frame_w: f32,
    frame_h: f32,
    // 0 HSV wheel, 1 motion vectors, 2 kinetic glow
    mode: u32,
    linear_output: u32,
    scale: f32,
    deadzone: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
// per grid cell: (dx, dy, confidence, unused)
@group(0) @binding(1) var<storage, read> cells: array<vec4<f32>>;
// the current frame (premultiplied linear RGBA), read by the kinetic glow only
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var<storage, read_write> outp: array<f32>;

const PI: f32 = 3.14159265358979;

fn cell_at(x: i32, y: i32) -> vec4<f32> {
    let cx = u32(clamp(x, 0, i32(p.grid_w) - 1));
    let cy = u32(clamp(y, 0, i32(p.grid_h) - 1));
    return cells[cy * p.grid_w + cx];
}

// `FlowField::sample_px`: bilinear (dx, dy, confidence) at full-frame pixel coordinates.
fn sample_px(px: f32, py: f32) -> vec3<f32> {
    let gx = px * (f32(p.grid_w) / p.frame_w) - 0.5;
    let gy = py * (f32(p.grid_h) / p.frame_h) - 0.5;
    let x0 = floor(gx);
    let y0 = floor(gy);
    let fx = gx - x0;
    let fy = gy - y0;
    let a = cell_at(i32(x0), i32(y0));
    let b = cell_at(i32(x0) + 1, i32(y0));
    let c = cell_at(i32(x0), i32(y0) + 1);
    let d = cell_at(i32(x0) + 1, i32(y0) + 1);
    let top = a.xyz + (b.xyz - a.xyz) * fx;
    let bot = c.xyz + (d.xyz - c.xyz) * fx;
    let r = top + (bot - top) * fy;
    return vec3<f32>(r.x, r.y, clamp(r.z, 0.0, 1.0));
}

fn rem_euclid(a: f32, b: f32) -> f32 {
    let r = a - b * floor(a / b);
    // floor rounding can land exactly on `b`
    return select(r, 0.0, r >= b);
}

// `hsv_to_linear_rgb`
fn hsv_to_linear_rgb(angle: f32, s: f32, v: f32) -> vec3<f32> {
    let deg = rem_euclid(angle * (180.0 / PI), 360.0);
    let c = v * s;
    let h6 = deg / 60.0;
    let x = c * (1.0 - abs(rem_euclid(h6, 2.0) - 1.0));
    let m = v - c;
    var rgb: vec3<f32>;
    if h6 < 1.0 {
        rgb = vec3<f32>(c, x, 0.0);
    } else if h6 < 2.0 {
        rgb = vec3<f32>(x, c, 0.0);
    } else if h6 < 3.0 {
        rgb = vec3<f32>(0.0, c, x);
    } else if h6 < 4.0 {
        rgb = vec3<f32>(0.0, x, c);
    } else if h6 < 5.0 {
        rgb = vec3<f32>(x, 0.0, c);
    } else {
        rgb = vec3<f32>(c, 0.0, x);
    }
    let u = clamp(rgb + vec3<f32>(m), vec3<f32>(0.0), vec3<f32>(1.0));
    return u * u;
}

// `filmcraft_color::linear_to_srgb` of a value clamped to 0..1
fn encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn src_rgb_clamped(x: i32, y: i32) -> vec3<f32> {
    let cx = clamp(x, 0, i32(p.out_w) - 1);
    let cy = clamp(y, 0, i32(p.out_h) - 1);
    return textureLoad(src, vec2<i32>(cx, cy), 0).rgb;
}

// `local_contrast_rgb`
fn local_contrast_rgb(x: i32, y: i32, conf: f32, act: f32) -> vec3<f32> {
    let c = src_rgb_clamped(x, y);
    let luma_c = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    var sum = c * 0.25;
    var w_sum = 0.25;
    for (var oy: i32 = -1; oy <= 1; oy = oy + 1) {
        for (var ox: i32 = -1; ox <= 1; ox = ox + 1) {
            if ox == 0 && oy == 0 {
                continue;
            }
            let ws = select(0.10, 0.15, ox == 0 || oy == 0);
            let n = src_rgb_clamped(x + ox, y + oy);
            let luma_n = 0.2126 * n.r + 0.7152 * n.g + 0.0722 * n.b;
            let dl = luma_n - luma_c;
            let w = ws / (1.0 + dl * dl * 36.0);
            sum = sum + n * w;
            w_sum = w_sum + w;
        }
    }
    let inv = 1.0 / w_sum;
    let clarity = 0.45 + 0.40 * conf * (1.0 - 0.35 * act);
    return max(c + (c - sum * inv) * clarity, vec3<f32>(0.0));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.out_w || id.y >= p.out_h {
        return;
    }
    let px = f32(id.x) + 0.5;
    let py = f32(id.y) + 0.5;
    let s = sample_px(px, py);
    let dx = s.x;
    let dy = s.y;
    let conf = s.z;
    let mag = sqrt(dx * dx + dy * dy);
    let act = clamp((mag - p.deadzone) / p.scale, 0.0, 1.0);
    var rgb = vec3<f32>(0.0);
    var encoded = false;
    if p.mode == 1u {
        // motion vectors: the codes are the encoded values themselves (the CPU's round trip
        // through the exact sRGB curve gives them back)
        let u = clamp(0.5 + 0.5 * clamp(dx / p.scale, -1.0, 1.0), 0.0, 1.0);
        let v = clamp(0.5 + 0.5 * clamp(dy / p.scale, -1.0, 1.0), 0.0, 1.0);
        let b = select(0.5, conf, act > 0.0);
        rgb = vec3<f32>(u, v, b);
        encoded = true;
    } else if p.mode == 3u {
        if textureLoad(src, vec2<i32>(id.xy), 0).a > 0.0 {
            rgb = local_contrast_rgb(i32(id.x), i32(id.y), conf, act);
        }
    } else if act > 0.0 {
        let angle = atan2(dy, dx);
        if p.mode == 0u {
            let v = clamp(act * (2.0 - act) * (0.35 + 0.65 * conf), 0.0, 1.0);
            rgb = hsv_to_linear_rgb(angle, 0.92, v);
        } else {
            let neon = hsv_to_linear_rgb(angle, 0.85, 1.0);
            let src_px = textureLoad(src, vec2<i32>(i32(id.x), i32(id.y)), 0);
            let src_luma = clamp(0.2126 * src_px.r + 0.7152 * src_px.g + 0.0722 * src_px.b, 0.0, 2.0);
            let energy = clamp(sqrt(act) * (0.25 + 0.75 * src_luma), 0.0, 1.0);
            rgb = (src_px.rgb * 0.35 + neon * 0.95) * energy;
        }
    }
    let i = (id.y * p.out_w + id.x) * 3u;
    if encoded || p.linear_output != 0u {
        outp[i] = rgb.r;
        outp[i + 1u] = rgb.g;
        outp[i + 2u] = rgb.b;
    } else {
        outp[i] = encode(rgb.r);
        outp[i + 1u] = encode(rgb.g);
        outp[i + 2u] = encode(rgb.b);
    }
}

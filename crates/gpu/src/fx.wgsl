// FilmCraft GPU effect stage: fused point operations, tiled/direct/running-sum spatial passes,
// reading the working image (`src`, linear premultiplied RGBA f32) and writing `dst`.
// The math mirrors `FxOp::apply` (the CPU reference) operation by
// operation; parameters arrive evaluated (keyframes, defaults and clamps applied on the CPU).

struct U {
    i0: vec4<u32>,  // op, width, height, unused
    i1: vec4<u32>,  // integer parameters (box radius, repeat, steps, channel, rect…)
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    p4: vec4<f32>,
    p5: vec4<f32>,
};

struct Batch {
    header: vec4<u32>, // count, width, height, reserved
    ops: array<U, 16>,
};
@group(0) @binding(0) var<uniform> batch: Batch;
var<private> u: U;
var<private> lut_base: u32;
@group(0) @binding(6) var<storage, read> grade_data: array<vec4<f32>>;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba32float, write>;
// Unsharp Mask: the image before blurring.
@group(0) @binding(3) var aux: texture_2d<f32>;
@group(0) @binding(7) var mask_cov: texture_2d<f32>;

struct MaskInfo {
    start: u32,
    len: u32,
    mode: u32,
    inverted: u32,
    feather: f32,
    expansion: f32,
    opacity: f32,
    _pad: f32,
};


@group(0) @binding(4) var<storage, read> masks: array<MaskInfo>;
@group(0) @binding(5) var<storage, read> pts: array<vec2<f32>>;
fn falloff(s: f32, feather: f32) -> f32 {
    let w = max(feather, 1.0);
    let u = clamp(s / w + 0.5, 0.0, 1.0);
    let smooth_u = u * u * (3.0 - 2.0 * u);
    return u + (smooth_u - u) * min(feather, 1.0);
}

// MaskMode index: None 0, Add 1, Subtract 2, Intersect 3, Lighten 4, Darken 5, Difference 6.
fn mode_start(mode: u32) -> f32 {
    if (mode == 2u || mode == 3u || mode == 5u) {
        return 1.0;
    }
    return 0.0;
}

fn combine(mode: u32, a: f32, m: f32) -> f32 {
    switch mode {
        case 1u: { return a + m - a * m; }
        case 2u: { return a * (1.0 - m); }
        case 3u: { return a * m; }
        case 4u: { return max(a, m); }
        case 5u: { return min(a, m); }
        case 6u: { return a + m - 2.0 * a * m; }
        default: { return a; }
    }
}

fn seg_dist2(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = b - a;
    let q = p - a;
    let l2 = dot(d, d);
    var t = 0.0;
    if (l2 > 0.0) {
        t = clamp(dot(q, d) / l2, 0.0, 1.0);
    }
    let e = q - d * t;
    return dot(e, e);
}

fn mask_value(m: MaskInfo, p: vec2<f32>) -> f32 {
    let band = abs(m.expansion) + max(m.feather, 1.0) * 0.5 + 1.0;
    var sd = -band;
    if (m.len >= 3u) {
        var wind = 0;
        var d2 = band * band;
        for (var i = 0u; i < m.len; i = i + 1u) {
            let a = pts[m.start + i];
            let c = pts[m.start + (i + 1u) % m.len];
            if ((a.y <= p.y) != (c.y <= p.y)) {
                let t = (p.y - a.y) / (c.y - a.y);
                if (a.x + (c.x - a.x) * t < p.x) {
                    if (c.y > a.y) { wind = wind + 1; } else { wind = wind - 1; }
                }
            }
            d2 = min(d2, seg_dist2(p, a, c));
        }
        let d = sqrt(d2);
        if (wind != 0) { sd = d; } else { sd = -d; }
    }
    let c = falloff(sd + m.expansion, m.feather);
    if (m.inverted != 0u) {
        return m.opacity * (1.0 - c);
    }
    return m.opacity * c;
}


fn coverage(p: vec2<f32>) -> f32 {
    if batch.header.w != 0u { return textureLoad(mask_cov,vec2<i32>(p),0).r; }
    var acc = mode_start(masks[0].mode);
    for (var i = 0u; i < u.i1.x; i++) {
        let m = masks[i];
        acc = clamp(combine(m.mode, acc, mask_value(m, p)), 0.0, 1.0);
    }
    return acc;
}

const OP_BRIGHTNESS_CONTRAST: u32 = 1u;
const OP_PROC_AMP: u32 = 2u;
const OP_TINT: u32 = 3u;
const OP_BLACK_WHITE: u32 = 4u;
const OP_COLOR_BALANCE: u32 = 5u;
const OP_LEAVE_COLOR: u32 = 6u;
const OP_CHANGE_TO_COLOR: u32 = 7u;
const OP_COLOR_PASS: u32 = 8u;
const OP_GAMMA: u32 = 9u;
const OP_LEVELS: u32 = 10u;
const OP_EXTRACT: u32 = 11u;
const OP_INVERT: u32 = 12u;
const OP_INVERT_ALPHA: u32 = 13u;
const OP_POSTERIZE: u32 = 14u;
const OP_ASC_CDL: u32 = 15u;
const OP_CHANNEL_MIX: u32 = 16u;
const OP_COLOR_REPLACE: u32 = 17u;
const OP_ALPHA_ADJUST: u32 = 18u;
const OP_VIGNETTE: u32 = 19u;
const OP_BOX: u32 = 20u;
const OP_DIRECTIONAL: u32 = 22u;
const OP_UNSHARP: u32 = 23u;
const OP_CROP: u32 = 24u;
const OP_RESAMPLE: u32 = 25u;
const OP_HFLIP: u32 = 26u;
const OP_VFLIP: u32 = 27u;
const OP_MIRROR: u32 = 28u;
const OP_OFFSET: u32 = 29u;
const OP_VIDEO_LIMITER: u32 = 30u;
const OP_LUMETRI: u32 = 31u;
const OP_CHROMA_KEY: u32 = 32u;
const OP_LUMA_KEY: u32 = 33u;
const OP_ULTRA_KEY: u32 = 34u;

fn size() -> vec2<i32> {
    return vec2<i32>(i32(u.i0.y), i32(u.i0.z));
}

fn ld(p: vec2<i32>) -> vec4<f32> {
    return textureLoad(src, p, 0);
}

// `Image::get_or_clear`
fn ld_or_clear(p: vec2<i32>) -> vec4<f32> {
    let s = size();
    if p.x < 0 || p.y < 0 || p.x >= s.x || p.y >= s.y {
        return vec4(0.0);
    }
    return textureLoad(src, p, 0);
}

// `Image::sample_bilinear`: pixel centres at +0.5, transparent outside.
fn sample_bilinear(x: f32, y: f32) -> vec4<f32> {
    let fx = x - 0.5;
    let fy = y - 0.5;
    let x0 = floor(fx);
    let y0 = floor(fy);
    let tx = fx - x0;
    let ty = fy - y0;
    let i = vec2<i32>(i32(x0), i32(y0));
    let a = ld_or_clear(i);
    let b = ld_or_clear(i + vec2(1, 0));
    let c = ld_or_clear(i + vec2(0, 1));
    let d = ld_or_clear(i + vec2(1, 1));
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    return top + (bot - top) * ty;
}

// `Image::sample_bilinear_clamped`
fn sample_clamped(x: f32, y: f32) -> vec4<f32> {
    let s = size();
    let fx = clamp(x - 0.5, 0.0, f32(s.x) - 1.0);
    let fy = clamp(y - 0.5, 0.0, f32(s.y) - 1.0);
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, s.x - 1);
    let y1 = min(y0 + 1, s.y - 1);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let a = ld(vec2(x0, y0));
    let b = ld(vec2(x1, y0));
    let c = ld(vec2(x0, y1));
    let d = ld(vec2(x1, y1));
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    return top + (bot - top) * ty;
}

// ---- colour helpers (`filmcraft_color`, `effects::enc` / `dec`)

fn linear_to_srgb1(v: f32) -> f32 {
    if v <= 0.0031308 {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn srgb_to_linear1(v: f32) -> f32 {
    if v <= 0.04045 {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn enc(c: vec3<f32>) -> vec3<f32> {
    let m = max(c, vec3(0.0));
    return vec3(linear_to_srgb1(m.x), linear_to_srgb1(m.y), linear_to_srgb1(m.z));
}

fn dec(c: vec3<f32>) -> vec3<f32> {
    let m = clamp(c, vec3(0.0), vec3(1.0));
    return vec3(srgb_to_linear1(m.x), srgb_to_linear1(m.y), srgb_to_linear1(m.z));
}

fn luma709(c: vec3<f32>) -> f32 {
    return 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
}

// `f32::powf` for x >= 0 (WGSL leaves pow(0, y) undefined).
fn powf(x: f32, y: f32) -> f32 {
    if x == 0.0 {
        if y == 0.0 {
            return 1.0;
        }
        if y > 0.0 {
            return 0.0;
        }
        return 3.0e38;
    }
    return pow(x, y);
}

// `f32::round`: halves away from zero (WGSL `round` rounds them to even).
fn round_away(x: f32) -> f32 {
    let a = abs(x);
    let f = floor(a);
    let r = select(f, f + 1.0, a - f >= 0.5);
    return select(r, -r, x < 0.0);
}

// `rem_euclid(1.0)`
fn fract_euclid(x: f32) -> f32 {
    return x - floor(x);
}

// `vfx::smoothstep` (not WGSL's: a degenerate edge pair divides by 1e-6)
fn smoothstep_fx(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / max(e1 - e0, 1e-6), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let r = c.x; let g = c.y; let b = c.z;
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let l = (mx + mn) / 2.0;
    if abs(mx - mn) < 1e-7 {
        return vec3(0.0, 0.0, l);
    }
    let d = mx - mn;
    var s: f32;
    if l > 0.5 {
        s = d / (2.0 - mx - mn);
    } else {
        s = d / (mx + mn);
    }
    var h: f32;
    if mx == r {
        h = (g - b) / d + select(0.0, 6.0, g < b);
    } else if mx == g {
        h = (b - r) / d + 2.0;
    } else {
        h = (r - g) / d + 4.0;
    }
    return vec3(h / 6.0, s, l);
}

fn hsl_channel(p: f32, q: f32, t0: f32) -> f32 {
    let t = fract_euclid(t0);
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if s <= 0.0 {
        return vec3(l);
    }
    var q: f32;
    if l < 0.5 {
        q = l * (1.0 + s);
    } else {
        q = l + s - l * s;
    }
    let p = 2.0 * l - q;
    return vec3(hsl_channel(p, q, h + 1.0 / 3.0), hsl_channel(p, q, h), hsl_channel(p, q, h - 1.0 / 3.0));
}

fn linear_to_enc_limiter(v: f32) -> f32 {
    if v <= 1.0 {
        return linear_to_srgb1(max(v, 0.0));
    }
    return 1.0 + (v - 1.0) / 2.4;
}

fn enc_to_linear_limiter(v: f32) -> f32 {
    if v <= 1.0 {
        return srgb_to_linear1(max(v, 0.0));
    }
    return 1.0 + (v - 1.0) * 2.4;
}

fn knee_limiter(v: f32, max_val: f32, comp: f32) -> f32 {
    let k = max_val * (1.0 - comp);
    if comp <= 0.0 || v <= k {
        return min(v, max_val);
    }
    let r = max_val - k;
    return k + r * (1.0 - exp(-(v - k) / r));
}

fn to_rgb_709(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let kr = 0.2126;
    let kb = 0.0722;
    let kg = 1.0 - kr - kb;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / kg;
    return vec3(r, g, b);
}

fn limit_video(v: vec3<f32>, max_val: f32, comp: f32, axis: u32) -> vec3<f32> {
    let kr = 0.2126;
    let kb = 0.0722;
    let kg = 1.0 - kr - kb;
    var y = kr * v.x + kg * v.y + kb * v.z;
    var cb = (v.z - y) / (2.0 * (1.0 - kb));
    var cr = (v.x - y) / (2.0 * (1.0 - kr));

    if axis != 1u {
        y = knee_limiter(max(y, 0.0), max_val, comp);
    }

    if axis != 0u {
        let rgb = to_rgb_709(y, cb, cr);
        var s = 1.0;
        let d = rgb - vec3(y);

        if rgb.x > max_val && d.x > 1e-6 {
            s = min(s, max((max_val - y) / d.x, 0.0));
        }
        if rgb.x < 0.0 && d.x < -1e-6 {
            s = min(s, max(y / (-d.x), 0.0));
        }

        if rgb.y > max_val && d.y > 1e-6 {
            s = min(s, max((max_val - y) / d.y, 0.0));
        }
        if rgb.y < 0.0 && d.y < -1e-6 {
            s = min(s, max(y / (-d.y), 0.0));
        }

        if rgb.z > max_val && d.z > 1e-6 {
            s = min(s, max((max_val - y) / d.z, 0.0));
        }
        if rgb.z < 0.0 && d.z < -1e-6 {
            s = min(s, max(y / (-d.z), 0.0));
        }

        if axis == 3u && s < 1.0 - 1e-4 {
            s = min(knee_limiter(s, 1.0, max(comp, 0.03)), 1.0);
        }
        cb *= s;
        cr *= s;
    }

    let out_rgb = to_rgb_709(y, cb, cr);
    return clamp(out_rgb, vec3(0.0), vec3(max_val));
}

fn lut_at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let n = u32(grade_data[lut_base].y);
    return grade_data[lut_base + 5u + u32(grade_data[lut_base].x) + r + g * n + b * n * n].xyz;
}

fn tetra(c: vec3<f32>) -> vec3<f32> {
    let n = u32(grade_data[lut_base].y);
    let m = f32(n - 1u);
    let t = clamp((c - grade_data[lut_base + 3u].xyz) / (grade_data[lut_base + 4u].xyz - grade_data[lut_base + 3u].xyz), vec3(0.0), vec3(1.0)) * m;
    let i = min(vec3<u32>(t), vec3(n - 2u));
    let f = t - vec3<f32>(i);
    let fr = f.x;
    let fg = f.y;
    let fb = f.z;
    let c000 = lut_at(i.x, i.y, i.z);
    let c111 = lut_at(i.x + 1u, i.y + 1u, i.z + 1u);
    var ca: vec3<f32>;
    var cb: vec3<f32>;
    var w: vec4<f32>;
    if (fr > fg) {
        if (fg > fb) {
            ca = lut_at(i.x + 1u, i.y, i.z); cb = lut_at(i.x + 1u, i.y + 1u, i.z); w = vec4(1.0 - fr, fr - fg, fg - fb, fb);
        } else if (fr > fb) {
            ca = lut_at(i.x + 1u, i.y, i.z); cb = lut_at(i.x + 1u, i.y, i.z + 1u); w = vec4(1.0 - fr, fr - fb, fb - fg, fg);
        } else {
            ca = lut_at(i.x, i.y, i.z + 1u); cb = lut_at(i.x + 1u, i.y, i.z + 1u); w = vec4(1.0 - fb, fb - fr, fr - fg, fg);
        }
    } else if (fb > fg) {
        ca = lut_at(i.x, i.y, i.z + 1u); cb = lut_at(i.x, i.y + 1u, i.z + 1u); w = vec4(1.0 - fb, fb - fg, fg - fr, fr);
    } else if (fb > fr) {
        ca = lut_at(i.x, i.y + 1u, i.z); cb = lut_at(i.x, i.y + 1u, i.z + 1u); w = vec4(1.0 - fg, fg - fb, fb - fr, fr);
    } else {
        ca = lut_at(i.x, i.y + 1u, i.z); cb = lut_at(i.x + 1u, i.y + 1u, i.z); w = vec4(1.0 - fg, fg - fr, fr - fb, fb);
    }
    return w.x * c000 + w.y * ca + w.z * cb + w.w * c111;
}


fn apply_lut(c: vec3<f32>, base: u32) -> vec3<f32> {
    lut_base = base;
    var v = c;
    let n = u32(grade_data[base].x);
    if n > 0u {
        let t = clamp((v - grade_data[base + 1u].xyz) / (grade_data[base + 2u].xyz - grade_data[base + 1u].xyz), vec3(0.0), vec3(1.0)) * f32(n - 1u);
        let i = min(vec3<u32>(t), vec3(n - 2u));
        for (var k = 0u; k < 3u; k++) {
            let lo = grade_data[base + 5u + i[k]][k];
            let hi = grade_data[base + 5u + i[k] + 1u][k];
            v[k] = lo + (hi - lo) * (t[k] - f32(i[k]));
        }
    }
    if grade_data[base].y > 0.0 { v = tetra(v); }
    return v;
}
fn has_curve(i: u32) -> bool { return (u.i1.x & (1u << i)) != 0u; }
fn curve_sample(i: u32, x: f32) -> f32 {
    let p = clamp(x, 0.0, 1.0) * 1023.0;
    let k = u32(p); let base = u.i1.w + i * 1024u;
    let lo = grade_data[base + k].x; let hi = grade_data[base + min(k + 1u, 1023u)].x;
    return lo + (hi - lo) * (p - f32(k));
}
fn look_curve(c: vec3<f32>, k: f32) -> vec3<f32> {
    let q = clamp(c, vec3(0.0), vec3(1.0));
    return q + (q * q * (3.0 - 2.0 * q) - q) * k;
}
fn procedural_look(look: u32, c: vec3<f32>) -> vec3<f32> {
    let l = luma709(c);
    switch look {
        case 1u: { return look_curve(c + vec3(0.0, 0.08, 0.1) * (1.0 - l) + vec3(0.1, 0.04, -0.06) * l, 0.35); }
        case 2u: { let v = c * vec3(1.06, 1.0, 0.9) + vec3(0.02, 0.01, 0.0); return (v + (vec3(l) - v) * 0.15) * 0.94 + 0.04; }
        case 3u: { return look_curve(c * vec3(0.9, 0.98, 1.1) + vec3(0.0, 0.0, 0.02), 0.2); }
        case 4u: { return look_curve(c + (vec3(l) - c) * 0.55, 0.6); }
        case 5u: { return 0.08 + (c + (vec3(l) - c) * 0.25) * 0.84; }
        case 6u: { return look_curve(vec3(l), 0.3); }
        case 7u: { return look_curve(c * vec3(1.1, 1.02, 0.82) + vec3(0.03, 0.01, 0.0), 0.15); }
        case 8u: { return c * vec3(0.75, 0.85, 1.15) * 0.8; }
        default: { return c; }
    }
}
fn advanced_grade(c: vec3<f32>) -> vec3<f32> {
    var v = enc(c);
    if u.i1.y != 0u {
        var lk = procedural_look(u.i1.y, v);
        if u.i1.y == 9u { lk = apply_lut(clamp(v, vec3(0.0), vec3(1.0)), u.i1.w + 9216u); }
        v += (lk - v) * u.p0.x;
    }
    if u.i1.z != 0u {
        let l = clamp(luma709(v), 0.0, 1.0); let ws = (1.0 - l) * (1.0 - l); let wh = l * l; let wm = max(1.0 - ws - wh, 0.0);
        v += u.p1.xyz * ws; v += u.p2.xyz * wm; v *= 1.0 + u.p3.xyz * wh;
    }
    for (var k = 0u; k < 3u; k++) { if has_curve(0u) { v[k] = curve_sample(0u, v[k]); } }
    for (var k = 0u; k < 3u; k++) { if has_curve(k + 1u) { v[k] = curve_sample(k + 1u, v[k]); } }
    if (u.i1.x & 496u) != 0u {
        var h = rgb_to_hsl(clamp(v, vec3(0.0), vec3(1.0))); let h0 = h.x; let s0 = h.y; let l0 = h.z;
        if has_curve(5u) { h.x = fract_euclid(h.x + (curve_sample(5u, h0) - 0.5)); }
        var sm = 1.0;
        if has_curve(4u) { sm *= curve_sample(4u, h0) * 2.0; }
        if has_curve(7u) { sm *= curve_sample(7u, l0) * 2.0; }
        if has_curve(8u) { sm *= curve_sample(8u, s0) * 2.0; }
        h.y = clamp(h.y * sm, 0.0, 1.0);
        if has_curve(6u) { h.z = clamp(h.z + (curve_sample(6u, h0) - 0.5) * 0.5, 0.0, 1.0); }
        v = hsl_to_rgb(h.x, h.y, h.z);
    }
    return dec(v);
}
fn hsl_secondary(c: vec3<f32>, matte: f32) -> vec3<f32> {
    let v = clamp(enc(c), vec3(0.0), vec3(1.0)); var h = rgb_to_hsl(v);
    let dh = min(abs(h.x - u.p0.x), 1.0 - abs(h.x - u.p0.x)); let soft = u.p1.y;
    var m = (1.0 - clamp((dh - u.p0.y * 0.5) / soft, 0.0, 1.0)) * clamp((h.y - u.p0.z) / soft, 0.0, 1.0) * min(clamp((h.z - u.p0.w) / soft, 0.0, 1.0), clamp((u.p1.x - h.z) / soft, 0.0, 1.0));
    if matte >= 0.0 { m = matte; }
    if u.i1.x == 1u { return dec(h.z + (v - h.z) * m); }
    if u.i1.x == 2u { return dec(v * m); }
    if u.i1.x == 3u { return dec(vec3(m)); }
    h.x = fract_euclid(h.x + u.p2.y); h.y = clamp(h.y * u.p2.x, 0.0, 1.0);
    let corrected = hsl_to_rgb(h.x, h.y, h.z) * vec3(1.0 + 0.25 * u.p1.z, 1.0 - 0.2 * u.p1.w, 1.0 - 0.25 * u.p1.z);
    return dec(v + (corrected - v) * m);
}
// ---- per-pixel colour effects on straight colour (`Image::map_rgb`)

fn color_op(op: u32, c: vec3<f32>) -> vec3<f32> {
    switch op {
        case OP_BRIGHTNESS_CONTRAST: {
            let br = u.p0.x; let co = u.p0.y;
            return dec((enc(c) - 0.5) * co + 0.5 + br);
        }
        case OP_PROC_AMP: {
            let br = u.p0.x; let co = u.p0.y; let hue = u.p0.z; let sat = u.p0.w;
            var hsl = rgb_to_hsl(enc(c));
            hsl.x = fract_euclid(hsl.x + hue);
            hsl.y = clamp(hsl.y * sat, 0.0, 1.0);
            return dec((hsl_to_rgb(hsl.x, hsl.y, hsl.z) - 0.5) * co + 0.5 + br);
        }
        case OP_TINT: {
            let bl = u.p0.xyz; let wh = u.p1.xyz; let amt = u.p0.w;
            let l = linear_to_srgb1(max(luma709(c), 0.0));
            let t = bl + (wh - bl) * l;
            let e = enc(c);
            return dec(e + (t - e) * amt);
        }
        case OP_BLACK_WHITE: {
            return vec3(luma709(c));
        }
        case OP_COLOR_BALANCE: {
            let e = enc(c);
            let l = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
            let ws = (1.0 - l) * (1.0 - l);
            let wh = l * l;
            let wm = 1.0 - ws - wh;
            var o = e + u.p0.xyz * ws + u.p1.xyz * max(wm, 0.0) + u.p2.xyz * wh;
            if u.i1.x != 0u {
                let l2 = 0.2126 * o.x + 0.7152 * o.y + 0.0722 * o.z;
                o = o + (l - l2);
            }
            return dec(o);
        }
        case OP_LEAVE_COLOR: {
            let amt = u.p0.x; let kh = u.p0.y; let tol = u.p0.z; let soft = u.p0.w;
            let h = rgb_to_hsl(enc(c)).x;
            let d = min(abs(h - kh), 1.0 - abs(h - kh)) * 2.0;
            let keep = 1.0 - clamp((d - tol) / soft, 0.0, 1.0);
            let l = luma709(c);
            let k = amt * (1.0 - keep);
            return c + (l - c) * k;
        }
        case OP_CHANGE_TO_COLOR: {
            let fh = u.p0.x; let th = u.p0.y; let tol = u.p0.z; let soft = u.p0.w;
            var hsl = rgb_to_hsl(enc(c));
            let d = min(abs(hsl.x - fh), 1.0 - abs(hsl.x - fh));
            let w = 1.0 - clamp((d - tol) / soft, 0.0, 1.0);
            hsl.x = fract_euclid(hsl.x + (th - fh) * w);
            return dec(hsl_to_rgb(hsl.x, hsl.y, hsl.z));
        }
        case OP_COLOR_PASS: {
            let e = enc(c);
            let k = e - u.p0.xyz;
            let d = sqrt(k.x * k.x + k.y * k.y + k.z * k.z);
            let passes = (d <= u.p0.w * 1.2) != (u.i1.x != 0u);
            if passes {
                return c;
            }
            return vec3(luma709(c));
        }
        case OP_GAMMA: {
            let e = enc(c);
            let g = u.p0.x;
            return dec(vec3(powf(max(e.x, 0.0), g), powf(max(e.y, 0.0), g), powf(max(e.z, 0.0), g)));
        }
        case OP_LEVELS: {
            let ib = u.p0.x; let iw = u.p0.y; let ob = u.p0.z; let ow = u.p0.w; let g = u.p1.x;
            let v = clamp((enc(c) - ib) / (iw - ib), vec3(0.0), vec3(1.0));
            return dec(ob + vec3(powf(v.x, g), powf(v.y, g), powf(v.z, g)) * (ow - ob));
        }
        case OP_EXTRACT: {
            let lo = u.p0.x; let hi = u.p0.y; let soft = u.p0.z;
            let l = linear_to_srgb1(max(luma709(c), 0.0));
            let inside = min(clamp((l - lo) / soft, 0.0, 1.0), clamp((hi - l) / soft, 0.0, 1.0));
            return vec3(select(inside, 1.0 - inside, u.i1.x != 0u));
        }
        case OP_INVERT: {
            let ch = u.i1.x;
            let blend = u.p0.x;
            let e = enc(c);
            var o = e;
            if ch == 0u || ch == 1u { o.x = 1.0 - e.x; }
            if ch == 0u || ch == 2u { o.y = 1.0 - e.y; }
            if ch == 0u || ch == 3u { o.z = 1.0 - e.z; }
            return dec(o + (e - o) * blend);
        }
        case OP_POSTERIZE: {
            let n = u.p0.x;
            let e = enc(c) * n;
            return dec(vec3(round_away(e.x), round_away(e.y), round_away(e.z)) / n);
        }
        case OP_ASC_CDL: {
            let e = enc(c);
            let s = u.p0.xyz; let o = u.p1.xyz; let pw = max(u.p2.xyz, vec3(0.0)); let sat = u.p0.w;
            let q = clamp(e * s + o, vec3(0.0), vec3(1.0));
            let v = vec3(powf(q.x, pw.x), powf(q.y, pw.y), powf(q.z, pw.z));
            let l = luma709(v);
            return dec(clamp(l + sat * (v - l), vec3(0.0), vec3(1.0)));
        }
        case OP_CHANNEL_MIX: {
            let v = enc(c);
            let r = u.p0; let g = u.p1; let b = u.p2;
            return dec(clamp(vec3(r.x * v.x + r.y * v.y + r.z * v.z + r.w, g.x * v.x + g.y * v.y + g.z * v.z + g.w, b.x * v.x + b.y * v.y + b.z * v.z + b.w), vec3(0.0), vec3(1.0)));
        }
        case OP_COLOR_REPLACE: {
            let v = enc(c);
            let t = u.p0.xyz; let sim = u.p0.w;
            let dd = v - t;
            let d = sqrt(dd.x * dd.x + dd.y * dd.y + dd.z * dd.z);
            let k = 1.0 - smoothstep_fx(sim * 0.85, max(sim, 1e-4), d);
            if k <= 0.0 {
                return c;
            }
            var repl = u.p1.xyz;
            if u.i1.x == 0u {
                let rh = u.p2.xyz;
                repl = hsl_to_rgb(rh.x, rh.y, rgb_to_hsl(v).z);
            }
            return dec(v + (repl - v) * k);
        }
        default: {
            return c;
        }
    }
}

fn lumetri_op(c: vec3<f32>, p: vec2<i32>) -> vec3<f32> {
    let s = size();
    let creative_on = u.i1.x != 0u;
    let vignette_on = u.i1.y != 0u;

    // white balance + exposure in linear light
    let ge = u.p0.xyz;
    let lin = c * ge;
    var v = enc(lin);

    // whites / blacks: endpoints
    let b0 = u.p0.w;
    let w0 = u.p1.x;
    let denom = max(w0 - b0, 1e-3);
    v = (v - vec3(b0)) / denom;

    // highlights / shadows: luma-weighted lift/compress, hue preserving
    let sh_k = u.p1.y;
    let hl_k = u.p1.z;
    let l = luma709(v);
    let ws_val = clamp(1.0 - l, 0.0, 1.0);
    let ws = ws_val * ws_val * ws_val;
    let whl_val = clamp(l, 0.0, 1.0);
    let whl = whl_val * whl_val * whl_val;
    let nl = max(l + sh_k * ws + hl_k * whl, 0.0);
    if l > 1e-5 {
        let k = nl / l;
        v = v * k;
    }

    // contrast: smooth S-curve around mid grey
    let contrast = u.p1.w;
    if abs(contrast) > 1e-4 {
        let k = 1.0 + contrast;
        let q = clamp(v, vec3(0.0), vec3(1.0));
        let sc = q * q * (vec3(3.0) - 2.0 * q);
        if k >= 1.0 {
            v = q + (sc - q) * (k - 1.0);
        } else {
            v = vec3(0.5) + (q - vec3(0.5)) * k;
        }
    }

    // faded film: lift blacks and compress
    let faded = u.p2.x;
    if faded > 0.0 {
        v = v * (1.0 - 0.25 * faded) + vec3(0.12 * faded);
    }

    // split tone
    if creative_on {
        let st_k = u.p4.xyz;
        let ht_k = u.p5.xyz;
        let l2 = clamp(luma709(v), 0.0, 1.0);
        v = v + st_k * (1.0 - l2) + ht_k * l2;
    }

    // saturation & vibrance
    let sat = u.p2.y;
    let vib = u.p2.z;
    let l3 = luma709(v);
    let cur_sat = max(max(v.x, v.y), v.z) - min(min(v.x, v.y), v.z);
    let sat_mult = sat * (1.0 + vib * (1.0 - clamp(cur_sat, 0.0, 1.0)));
    v = vec3(l3) + (v - vec3(l3)) * sat_mult;

    // vignette
    let va = u.p2.w;
    if vignette_on && abs(va) > 1e-4 {
        let vmid = u.p3.x;
        let vfeather = u.p3.y;
        let vround_aspect = u.p3.z;
        let w = f32(s.x);
        let h = f32(s.y);
        let nx = (f32(p.x) / w - 0.5) * 2.0 * vround_aspect;
        let ny = (f32(p.y) / h - 0.5) * 2.0;
        let d = sqrt(nx * nx + ny * ny) / 1.4142135623730951;
        let edge = clamp((d - vmid * 0.9) / (max(vfeather, 0.01) * 0.9), 0.0, 1.0);
        let e2 = edge * edge * (3.0 - 2.0 * edge);
        let k = 1.0 + va * 0.2 * e2;
        if va < 0.0 {
            v = v * max(k, 0.0);
        } else {
            v = v + (vec3(1.0) - v) * (k - 1.0);
        }
    }

    return dec(v);
}

// One output pixel of every op but the running-sum box blur.
fn pixel(op: u32, p: vec2<i32>, input: vec4<f32>) -> vec4<f32> {
    let s = size();
    let pc = vec2<f32>(p) + 0.5;
    switch op {
        case 37u, 38u, 39u: {
            let o = input; if o.a <= 1e-6 { return o; }
            let c = o.rgb / o.a; var result: vec3<f32>;
            if op == 37u { result = dec(apply_lut(clamp(enc(c), vec3(0.0), vec3(1.0)), u.i1.w)); }
            else if op == 38u { result = advanced_grade(c); }
            else { result = hsl_secondary(c, -1.0); }
            return vec4(result * o.a, o.a);
        }
        case 40u: {
            var m = 0.0;
            if input.a > 1e-6 {
                let v = clamp(enc(input.rgb / input.a), vec3(0.0), vec3(1.0)); let h = rgb_to_hsl(v);
                let dh = min(abs(h.x - u.p0.x), 1.0 - abs(h.x - u.p0.x)); let soft = u.p1.y;
                m = (1.0 - clamp((dh - u.p0.y * 0.5) / soft, 0.0, 1.0)) * clamp((h.y - u.p0.z) / soft, 0.0, 1.0) * min(clamp((h.z - u.p0.w) / soft, 0.0, 1.0), clamp((u.p1.x - h.z) / soft, 0.0, 1.0));
            }
            return vec4(vec3(m), 1.0);
        }
        case 41u: {
            let r = i32(u.i1.x); var window: array<f32, 49>; var count = 0u;
            for (var y = max(p.y - r, 0); y <= min(p.y + r, s.y - 1); y++) {
                for (var x = max(p.x - r, 0); x <= min(p.x + r, s.x - 1); x++) {
                    let value = ld(vec2(x, y)).x;
                    var at = count;
                    while at > 0u {
                        if window[at - 1u] <= value { break; }
                        window[at] = window[at - 1u]; at--;
                    }
                    window[at] = value; count++;
                }
            }
            let m = input.x + (window[count / 2u] - input.x) * min(u.p0.x, 1.0);
            return vec4(vec3(m), 1.0);
        }
        case 42u: {
            let o = textureLoad(aux, p, 0); if o.a <= 1e-6 { return o; }
            return vec4(hsl_secondary(o.rgb / o.a, input.x) * o.a, o.a);
        }
        case 35u: {
            let original = textureLoad(aux, p, 0);
            return original + (input - original) * coverage(pc);
        }
        case 36u: { return input * coverage(pc); }
        case OP_ULTRA_KEY: {
            let o = input;
            var v = vec3(0.0);
            var a = 0.0;
            if o.a > 1e-6 {
                v = enc(o.rgb / o.a);
                let y = 0.2126 * v.r + (1.0 - 0.2126 - 0.0722) * v.g + 0.0722 * v.b;
                let ycc = vec3(y, (v.b - y) / (2.0 * (1.0 - 0.0722)), (v.r - y) / (2.0 * (1.0 - 0.2126)));
                let k = u.p0.xyz;
                let kmag2 = u.p0.w;
                let proj = (ycc.y * k.y + ycc.z * k.z) / kmag2;
                let dy = ycc.y - proj * k.y;
                let dz = ycc.z - proj * k.z;
                let perp = sqrt(dy * dy + dz * dz) / sqrt(kmag2);
                let keyness = clamp(proj, 0.0, 1.5) * (1.0 - smoothstep_fx(0.0, u.p1.x, perp));
                var m = clamp(1.0 - keyness * u.p1.y, 0.0, 1.0);
                m += max(y - k.x, 0.0) * u.p1.w * 2.0 * min(keyness, 1.0);
                m += max(k.x - y, 0.0) * u.p2.x * 2.0 * min(keyness, 1.0);
                a = clamp((clamp(m, 0.0, 1.0) - u.p1.z) / (1.0 - u.p1.z), 0.0, 1.0);
            }
            if u.p2.y > 0.0 { a = clamp((a - u.p2.z) * (1.0 + u.p2.y * 4.0) + u.p2.z, 0.0, 1.0); }
            let dom = u.i1.x;
            let o1 = v[(dom + 1u) % 3u]; let o2 = v[(dom + 2u) % 3u];
            let limit = max(o1, o2) * (1.0 - u.p3.y) + (o1 + o2) * 0.5 * u.p3.y;
            let excess = max(v[dom] - limit, 0.0);
            if excess > 0.0 && u.p2.w > 0.0 {
                let l0 = luma709(v);
                v[dom] -= excess * u.p2.w;
                let l1 = luma709(v);
                v = l1 + (v - l1) * (1.0 - u.p3.x * min(excess * 4.0, 1.0));
                v += vec3((l0 - l1) * u.p3.z);
            }
            if abs(u.p3.w - 1.0) > 1e-4 || abs(u.p4.x) > 1e-6 || abs(u.p4.y - 1.0) > 1e-4 {
                var hsl = rgb_to_hsl(clamp(v, vec3(0.0), vec3(1.0)));
                hsl.x = fract(hsl.x + u.p4.x);
                hsl.y = clamp(hsl.y * u.p3.w, 0.0, 1.0);
                hsl.z = clamp(hsl.z * u.p4.y, 0.0, 1.0);
                v = hsl_to_rgb(hsl.x, hsl.y, hsl.z);
            }
            if u.i1.y == 1u { return vec4(vec3(srgb_to_linear1(a) * o.a), o.a); }
            let na = select(a * o.a, o.a, u.i1.y == 2u);
            return vec4(dec(v) * na, na);
        }
        case OP_CHROMA_KEY: {
            let o = input;
            if o.a <= 0.0 { return o; }
            var c = enc(o.rgb / o.a);
            // `rgb_to_ycbcr`, BT.709, on display-encoded straight colour.
            let y = 0.2126 * c.r + (1.0 - 0.2126 - 0.0722) * c.g + 0.0722 * c.b;
            let ycc = vec3(y, (c.b - y) / (2.0 * (1.0 - 0.0722)), (c.r - y) / (2.0 * (1.0 - 0.2126)));
            let delta = ycc - u.p0.xyz;
            let d = sqrt(delta.y * delta.y + delta.z * delta.z) + abs(delta.x) * 0.15;
            let alpha = clamp((d - u.p0.w) / u.p1.x, 0.0, 1.0);
            let dom = u.i1.x;
            let spill = u.p1.y;
            if spill > 0.0 {
                let others = (c[(dom + 1u) % 3u] + c[(dom + 2u) % 3u]) / 2.0;
                if c[dom] > others { c[dom] -= (c[dom] - others) * spill; }
            }
            let a = o.a * alpha;
            if u.i1.y == 1u { return vec4(vec3(a), o.a); }
            return vec4(dec(c) * a, a);
        }
        case OP_LUMA_KEY: {
            let o = input;
            if o.a <= 0.0 { return o; }
            let l = linear_to_srgb1(max(luma709(o.rgb / o.a), 0.0));
            let th = u.p0.x;
            let cut = u.p0.y;
            var a = 0.0;
            if l > cut {
                a = 1.0;
                if l < max(th, cut + 1e-3) { a = (l - cut) / max(th - cut, 1e-3); }
            }
            return o * a;
        }
        case OP_INVERT_ALPHA: {
            let o = input;
            let a = o.a;
            let na = 1.0 - a;
            // (the CPU's threshold; `na / a` is only used when a > 1e-6)
            var k = 0.0;
            if a > 1e-6 {
                k = na / a;
            }
            let blend = u.p0.x;
            return vec4(o.rgb * k, na * (1.0 - blend) + a * blend);
        }
        case OP_ALPHA_ADJUST: {
            let o = input;
            let c = select(o.rgb / o.a, vec3(0.0), o.a <= 1e-6);
            var a = select(o.a, 1.0, u.i1.x != 0u);
            if u.i1.y != 0u {
                a = 1.0 - a;
            }
            a = clamp(a * u.p0.x, 0.0, 1.0);
            if u.i1.z != 0u {
                return vec4(vec3(srgb_to_linear1(a)), 1.0);
            }
            return vec4(c * a, a);
        }
        case OP_BOX: {
            // one box pass along x (i1.z = 0) or y (1): mean over 2r+1 pixels, clamped to the
            // edge (repeat) or skipping pixels outside, as `effects::box_rows`
            let r = i32(u.i1.x);
            let repeat = u.i1.y != 0u;
            let vertical = u.i1.z != 0u;
            let n = select(s.x, s.y, vertical);
            let x = select(p.x, p.y, vertical);
            var acc = vec4(0.0);
            for (var i = x - r; i <= x + r; i++) {
                if repeat || (i >= 0 && i < n) {
                    let j = clamp(i, 0, n - 1);
                    acc += ld(select(vec2(j, p.y), vec2(p.x, j), vertical));
                }
            }
            return acc * (1.0 / f32(2 * r + 1));
        }
        case OP_DIRECTIONAL: {
            let steps = u.i1.x;
            let dx = u.p0.x; let dy = u.p0.y;
            var acc = vec4(0.0);
            for (var i = 0u; i < steps; i++) {
                let t = f32(i) / f32(steps - 1u) - 0.5;
                acc += sample_clamped(pc.x + dx * t, pc.y + dy * t);
            }
            return acc / f32(steps);
        }
        case OP_UNSHARP: {
            // src: the blurred image, aux: the original
            var o = textureLoad(aux, p, 0);
            let bq = ld(p);
            let amount = u.p0.x; let th = u.p0.y;
            for (var k = 0; k < 3; k++) {
                let d = o[k] - bq[k];
                if abs(d) >= th * o.a {
                    o[k] = max(o[k] + d * amount, 0.0);
                }
            }
            return o;
        }
        case OP_CROP: {
            let x0 = u.p0.x; let x1 = u.p0.y; let y0 = u.p0.z; let y1 = u.p0.w; let fe = u.p1.x;
            let d = min(min(pc.x - x0, x1 - pc.x), min(pc.y - y0, y1 - pc.y));
            var a: f32;
            if fe > 0.0 {
                a = clamp(d / fe, 0.0, 1.0);
            } else {
                a = clamp(d + 0.5, 0.0, 1.0);
            }
            let o = input;
            return select(o, o * a, a < 1.0);
        }
        case OP_RESAMPLE: {
            // inverse map p0 = (a b c d), p1 = (e f opacity valid); rect i1 = x0 x1 y0 y1
            if u.p1.w == 0.0 || u32(p.x) < u.i1.x || u32(p.x) >= u.i1.y || u32(p.y) < u.i1.z || u32(p.y) >= u.i1.w {
                return vec4(0.0);
            }
            let m0 = u.p0; let m1 = u.p1;
            let uu = m0.x * pc.x + m0.z * pc.y + m1.x;
            let vv = m0.y * pc.x + m0.w * pc.y + m1.y;
            if uu < -1.0 || vv < -1.0 || uu > f32(s.x) + 1.0 || vv > f32(s.y) + 1.0 {
                return vec4(0.0);
            }
            return sample_bilinear(uu, vv) * m1.z;
        }
        case OP_HFLIP: {
            return ld(vec2(s.x - 1 - p.x, p.y));
        }
        case OP_VFLIP: {
            return ld(vec2(p.x, s.y - 1 - p.y));
        }
        case OP_MIRROR: {
            let c = u.p0.xy; let nrm = u.p0.zw;
            let d = (pc.x - c.x) * nrm.x + (pc.y - c.y) * nrm.y;
            if d > 0.0 {
                return sample_bilinear(pc.x - 2.0 * d * nrm.x, pc.y - 2.0 * d * nrm.y);
            }
            return ld(p);
        }
        case OP_OFFSET: {
            let w = f32(s.x); let h = f32(s.y);
            // dx, dy in 0..w / 0..h, so the shifted coordinate is within (-w, w + 0.5)
            var uu = pc.x - u.p0.x;
            var vv = pc.y - u.p0.y;
            if uu < 0.0 { uu += w; }
            if uu >= w { uu -= w; }
            if vv < 0.0 { vv += h; }
            if vv >= h { vv -= h; }
            let q = sample_clamped(uu, vv);
            let o = input;
            return q + (o - q) * u.p0.z;
        }
        case OP_VIGNETTE: {
            let o = input;
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            let amt = u.p0.x;
            if abs(amt) < 1e-5 {
                return o;
            }
            let mid = u.p0.y;
            let round = u.p0.z;
            let feather = u.p0.w;
            let tgt = u.p1.xyz;
            let w = f32(s.x);
            let h = f32(s.y);
            let aspect = w / h;
            var nx = pc.x / w * 2.0 - 1.0;
            let ny = pc.y / h * 2.0 - 1.0;
            if round > 0.0 {
                nx *= 1.0 + (aspect - 1.0) * round;
            }
            let p_exp = select(2.0, 2.0 + (-round) * 6.0, round < 0.0);
            let d = powf(powf(abs(nx), p_exp) + powf(abs(ny), p_exp), 1.0 / p_exp) / powf(2.0, 1.0 / p_exp);
            let edge = smoothstep_fx(mid - feather * 0.5, mid + feather * 0.5, d);
            let c = o.rgb / a;
            let ec = enc(c);
            let res = dec(ec + (tgt - ec) * (edge * abs(amt)));
            return vec4(res * a, a);
        }
        case OP_VIDEO_LIMITER: {
            let o = input;
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            let c = o.rgb / a;
            let v = vec3(linear_to_enc_limiter(c.x), linear_to_enc_limiter(c.y), linear_to_enc_limiter(c.z));
            let max_val = u.p0.x;
            let comp = u.p0.y;
            let axis = u.i1.x;
            let warn = u.i1.y != 0u;
            let wc = u.p0.zw;
            let wc_b = u.p1.x;
            let warning_col = vec3(wc.x, wc.y, wc_b);
            let out_enc = limit_video(v, max_val, comp, axis);
            let diff = abs(v - out_enc);
            if warn && (diff.x > 1e-4 || diff.y > 1e-4 || diff.z > 1e-4) {
                return vec4(warning_col * a, a);
            }
            let res = vec3(enc_to_linear_limiter(out_enc.x), enc_to_linear_limiter(out_enc.y), enc_to_linear_limiter(out_enc.z));
            return vec4(res * a, a);
        }
        case OP_LUMETRI: {
            let o = input;
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            let c = o.rgb / a;
            let res = lumetri_op(c, p);
            return vec4(res * a, a);
        }
        default: {
            let o = input;
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            return vec4(color_op(op, o.rgb / a) * a, a);
        }
    }
}

@compute @workgroup_size(16, 16)
fn fx_px(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= batch.header.y || id.y >= batch.header.z {
        return;
    }
    let p = vec2<i32>(id.xy);
    var value = ld(p);
    for (var i = 0u; i < batch.header.x; i++) {
        u = batch.ops[i];
        value = pixel(u.i0.x, p, value);
    }
    textureStore(dst, p, value);
}

// A box pass as a running sum along one row (i1.z = 0) or column (1) per invocation: O(1) per
// pixel for any radius, the arithmetic of `effects::box_rows`.
@compute @workgroup_size(64)
fn fx_run(@builtin(global_invocation_id) id: vec3<u32>) {
    u = batch.ops[0];
    let s = size();
    let vertical = u.i1.z != 0u;
    let n = select(s.x, s.y, vertical);
    let lines = select(s.y, s.x, vertical);
    let line = i32(id.x);
    if line >= lines {
        return;
    }
    let r = i32(u.i1.x);
    let repeat = u.i1.y != 0u;
    let inv = 1.0 / f32(2 * r + 1);
    var acc = vec4(0.0);
    for (var i = -r; i <= r; i++) {
        if repeat || (i >= 0 && i < n) {
            let j = clamp(i, 0, n - 1);
            acc += ld(select(vec2(j, line), vec2(line, j), vertical));
        }
    }
    for (var x = 0; x < n; x++) {
        textureStore(dst, select(vec2(x, line), vec2(line, x), vertical), acc * inv);
        let out_i = x - r;
        let in_i = x + r + 1;
        if repeat || out_i >= 0 {
            let j = clamp(out_i, 0, n - 1);
            acc -= ld(select(vec2(j, line), vec2(line, j), vertical));
        }
        if repeat || in_i < n {
            let j = clamp(in_i, 0, n - 1);
            acc += ld(select(vec2(j, line), vec2(line, j), vertical));
        }
    }
}

// Cooperative inclusive prefix scan: a group loads 128 output samples plus its halo once.
// The scan is shared-memory O(log tile); each output window is one subtraction, independent
// of blur radius. Both axes preserve repeat/transparent edges and the original denominator.
var<workgroup> prefix: array<vec4<f32>,256>;
@compute @workgroup_size(128)
fn fx_tile(@builtin(workgroup_id) group:vec3<u32>, @builtin(local_invocation_index) lane:u32) {
    u = batch.ops[0];
    let vertical = u.i1.z != 0u;
    let n = select(i32(batch.header.y),i32(batch.header.z),vertical);
    let r = i32(u.i1.x);
    let base = i32(group.x)*128-r;
    let line = i32(group.y);
    for(var k=lane;k<256u;k+=128u) {
        let x = base+i32(k);
        var value = vec4(0.0);
        if k < 128u + 2u*u.i1.x && (u.i1.y != 0u || (x>=0 && x<n)) {
            let j=clamp(x,0,n-1);
            value=ld(select(vec2(j,line),vec2(line,j),vertical));
        }
        prefix[k]=value;
    }
    workgroupBarrier();
    for(var offset=1u;offset<256u;offset*=2u) {
        var a=vec4(0.0);var b=vec4(0.0);
        if lane>=offset {a=prefix[lane-offset];}
        if lane+128u>=offset {b=prefix[lane+128u-offset];}
        workgroupBarrier();
        prefix[lane]+=a;prefix[lane+128u]+=b;
        workgroupBarrier();
    }
    let x=i32(group.x)*128+i32(lane);
    if x>=n {return;}
    var acc=prefix[lane+2u*u.i1.x];
    if lane>0u {acc-=prefix[lane-1u];}
    textureStore(dst,select(vec2(x,line),vec2(line,x),vertical),acc/f32(2*r+1));
}

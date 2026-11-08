// The export's per-pixel work after rendering, for a frame that stays on the GPU (ADR 0003 Step 2):
// the CPU's `Pipeline::frame` and the encoder's colour conversion in one pass, step for step:
//
// 1. placement: the rendered picture (the compositor's premultiplied linear-light accumulator)
//    resampled to its size in the output frame (`pipeline::resample`: a triangle filter widened by
//    the reduction factor, weights normalised per output sample, width then height) at its offset;
//    outside it the frame is transparent black;
// 2. the output transfer function: the sRGB curve of the colour over black, or of the straight colour
//    when the export keeps alpha (`srgb_floats`), or the 8-bit table of `Image::over_black_rgba8` /
//    `Image::to_rgba8` for an encoder that codes 8 bits;
// 3. the video limiter on the encoded signal (`limit_px`; on the 8-bit picture it floors to 8 bits,
//    as `limit_rgba8` does);
// 4. R'G'B' -> limited-range Y'CbCr with the export's matrix (`convert.rs`): 10-bit codes 4..1019,
//    chroma 4:4:4 or averaged over each horizontal pair (the odd last column alone), and the alpha
//    plane as full-range 10-bit codes of the 8-bit alpha (`alpha_10` of `to_rgba8`'s alpha).
//
// One invocation per horizontal pixel pair and row writes the pair's samples into the planes the
// encoders read: 16-bit samples, two to a little-endian word, Y then Cb, Cr and alpha, one after
// the other (`intra_enc::pack_planes`). A word can hold samples of two invocations (odd widths,
// plane boundaries), so samples are OR-ed into the zeroed buffer.

struct Params {
    width: u32,
    height: u32,
    // chroma samples per row (the width, or half of it rounded up)
    cwidth: u32,
    flags: u32,
    // the rendered picture
    rw: u32,
    rh: u32,
    // the picture's size in the output frame and its top-left there
    cw: u32,
    ch: u32,
    ox: i32,
    oy: i32,
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
    // resampling: source samples per output sample, and the filter's half width
    scale_x: f32,
    scale_y: f32,
    support_x: f32,
    support_y: f32,
    lim_lo: f32,
    lim_hi: f32,
    kr: f32,
    kg: f32,
    kb: f32,
    // the chroma divisors 2 (1 - Kb) and 2 (1 - Kr)
    sb: f32,
    sr: f32,
    _p3: f32,
}

const RESAMPLE: u32 = 1u;
// encoded floats (else the 8-bit picture)
const FLOATS: u32 = 2u;
// straight colour and an alpha plane (else the colour over black)
const ALPHA: u32 = 4u;
const CHROMA444: u32 = 8u;
const LIMITER: u32 = 16u;

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var src: texture_2d<f32>;
// `linear_to_srgb_u8_table`, four entries to a word
@group(0) @binding(2) var<storage, read> lut: array<u32>;
@group(0) @binding(3) var<storage, read_write> planes: array<atomic<u32>>;

fn texel(x: i32, y: i32) -> vec4<f32> {
    return textureLoad(src, vec2<i32>(x, y), 0);
}

// One axis of `pipeline::weights`: the first and last source sample of output sample `i`, the
// filter's centre and half width, and the sum the weights are normalised by.
struct Taps {
    lo: i32,
    hi: i32,
    c: f32,
    support: f32,
    sum: f32,
}

fn taps(i: u32, n_src: u32, scale: f32, support: f32) -> Taps {
    let c = (f32(i) + 0.5) * scale - 0.5;
    let lo = max(i32(ceil(c - support)), 0);
    let hi = max(min(i32(floor(c + support)), i32(n_src) - 1), lo);
    var sum = 0.0;
    for (var j = lo; j <= hi; j++) {
        sum += max(1.0 - abs(f32(j) - c) / support, 0.0);
    }
    return Taps(lo, hi, c, support, sum);
}

fn weight(t: Taps, j: i32) -> f32 {
    if t.sum > 0.0 {
        return max(1.0 - abs(f32(j) - t.c) / t.support, 0.0) / t.sum;
    }
    return select(0.0, 1.0, j == t.lo);
}

fn resampled(x: u32, y: u32) -> vec4<f32> {
    let tx = taps(x, P.rw, P.scale_x, P.support_x);
    let ty = taps(y, P.rh, P.scale_y, P.support_y);
    var acc = vec4<f32>(0.0);
    for (var i = ty.lo; i <= ty.hi; i++) {
        var row = vec4<f32>(0.0);
        for (var j = tx.lo; j <= tx.hi; j++) {
            row += texel(j, i) * weight(tx, j);
        }
        acc += row * weight(ty, i);
    }
    return acc;
}

// The premultiplied linear colour of output pixel (x, y).
fn placed(x: u32, y: u32) -> vec4<f32> {
    let sx = i32(x) - P.ox;
    let sy = i32(y) - P.oy;
    if sx < 0 || sy < 0 || sx >= i32(P.cw) || sy >= i32(P.ch) {
        return vec4<f32>(0.0);
    }
    if (P.flags & RESAMPLE) == 0u {
        return texel(sx, sy);
    }
    return resampled(u32(sx), u32(sy));
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

// `srgb_floats`' channel encoding (NaN and negative values give 0).
fn encode(v: f32) -> f32 {
    if v > 0.0 {
        return linear_to_srgb(min(v, 1.0));
    }
    return 0.0;
}

// `linear_to_srgb_u8`: the table entry at `linear_to_srgb_index`.
fn srgb8(v: f32) -> f32 {
    let i = u32(i32(clamp(v, 0.0, 1.0) * 4095.0 + 0.5)) & 4095u;
    return f32((lut[i >> 2u] >> ((i & 3u) * 8u)) & 255u);
}

// `limit_px`: BT.709 luma of the encoded signal within [lo, hi].
fn limit(c: vec3<f32>) -> vec3<f32> {
    let lo = P.lim_lo;
    let hi = P.lim_hi;
    var o = c;
    let y = 0.2126 * o.x + 0.7152 * o.y + 0.0722 * o.z;
    if y > hi && y > 0.0 {
        o = o * (hi / y);
    } else if y < lo {
        o = o + vec3<f32>(lo - y);
    }
    return clamp(o, vec3<f32>(0.0), vec3<f32>(max(hi, lo)));
}

// The encoded R'G'B' (0..1) the encoder converts, and the pixel's 8-bit alpha.
struct Signal {
    rgb: vec3<f32>,
    alpha: u32,
}

fn signal(p: vec4<f32>) -> Signal {
    let straight = (P.flags & ALPHA) != 0u;
    let a = clamp(p.w, 0.0, 1.0);
    // `to_rgba8`'s alpha byte
    var alpha = 0u;
    if a > 0.0 {
        alpha = u32(a * 255.0 + 0.5);
    }
    var rgb: vec3<f32>;
    if (P.flags & FLOATS) != 0u {
        var k = 1.0;
        if straight {
            k = select(0.0, 1.0 / min(p.w, 1.0), p.w > 0.0);
        }
        rgb = vec3<f32>(encode(p.x * k), encode(p.y * k), encode(p.z * k));
        if (P.flags & LIMITER) != 0u {
            rgb = limit(rgb);
        }
        return Signal(rgb, alpha);
    }
    // the 8-bit picture: over black, or straight next to its alpha (zero where transparent)
    var bytes = vec3<f32>(0.0);
    if !straight {
        bytes = vec3<f32>(srgb8(p.x), srgb8(p.y), srgb8(p.z));
    } else if a > 0.0 {
        let inv = 1.0 / a;
        bytes = vec3<f32>(srgb8(p.x * inv), srgb8(p.y * inv), srgb8(p.z * inv));
    }
    if (P.flags & LIMITER) != 0u {
        bytes = clamp(floor(limit(bytes / 255.0) * 255.0), vec3<f32>(0.0), vec3<f32>(255.0));
    }
    return Signal(bytes / 255.0, alpha);
}

// `x.round().clamp(4.0, 1019.0)` for the values the conversion makes.
fn code10(x: f32) -> u32 {
    return u32(clamp(floor(x + 0.5), 4.0, 1019.0));
}

fn luma(c: vec3<f32>) -> f32 {
    return P.kr * c.x + P.kg * c.y + P.kb * c.z;
}

fn put(sample: u32, v: u32) {
    atomicOr(&planes[sample >> 1u], (v & 0xffffu) << ((sample & 1u) * 16u));
}

@compute @workgroup_size(8, 8, 1)
fn post(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x0 = gid.x * 2u;
    let y = gid.y;
    if x0 >= P.width || y >= P.height {
        return;
    }
    let pair = x0 + 1u < P.width;
    let s0 = signal(placed(x0, y));
    var s1 = s0;
    if pair {
        s1 = signal(placed(x0 + 1u, y));
    }
    let y0 = luma(s0.rgb);
    let y1 = luma(s1.rgb);
    let row = y * P.width;
    put(row + x0, code10(64.0 + 876.0 * y0));
    if pair {
        put(row + x0 + 1u, code10(64.0 + 876.0 * y1));
    }
    let u0 = (s0.rgb.z - y0) / P.sb;
    let v0 = (s0.rgb.x - y0) / P.sr;
    let u1 = (s1.rgb.z - y1) / P.sb;
    let v1 = (s1.rgb.x - y1) / P.sr;
    if (P.flags & CHROMA444) != 0u {
        put(P.off_cb + row + x0, code10(512.0 + 896.0 * u0));
        put(P.off_cr + row + x0, code10(512.0 + 896.0 * v0));
        if pair {
            put(P.off_cb + row + x0 + 1u, code10(512.0 + 896.0 * u1));
            put(P.off_cr + row + x0 + 1u, code10(512.0 + 896.0 * v1));
        }
    } else {
        // (the odd last column is averaged with itself)
        let c = y * P.cwidth + gid.x;
        put(P.off_cb + c, code10(512.0 + 896.0 * (u0 + u1) * 0.5));
        put(P.off_cr + c, code10(512.0 + 896.0 * (v0 + v1) * 0.5));
    }
    if (P.flags & ALPHA) != 0u {
        put(P.off_a + row + x0, (s0.alpha * 1023u + 127u) / 255u);
        if pair {
            put(P.off_a + row + x0 + 1u, (s1.alpha * 1023u + 127u) / 255u);
        }
    }
}

// FilmCraft GPU Optical Flow: `filmcraft_render::flow::interpolate` (the CPU reference) as compute
// passes, step by step: the working luma, its pyramid, Lucas–Kanade refinement per level, the
// splat of both flows to the in-between time and its hole filling, then the warp and blend at the
// working image's size. Every pass writes one texture (`out`); the splat passes use the buffers.
//
// Images are rgba32float: luma in .r, a flow in .rg (the in-between flow carries its validity in
// .b). Sizes come from `textureDimensions`; i0.yz is the size written.

struct U {
    i0: vec4<u32>,  // op, width, height, unused
    i1: vec4<u32>,  // integer parameters
    p0: vec4<f32>,  // t, scale, flow::DAMP, flow::STILL_SLACK
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var t0: texture_2d<f32>;
@group(0) @binding(2) var t1: texture_2d<f32>;
@group(0) @binding(3) var t2: texture_2d<f32>;
@group(0) @binding(4) var t3: texture_2d<f32>;
@group(0) @binding(5) var t4: texture_2d<f32>;
@group(0) @binding(6) var out: texture_storage_2d<rgba32float, write>;
// splat: per target pixel the best score (bitwise not of its bits, 0 = none) and the winning
// source (bitwise not of its order, 0 = none); per source pixel its score bits and target cell
@group(0) @binding(7) var<storage, read_write> best: array<atomic<u32>>;
@group(0) @binding(8) var<storage, read_write> winner: array<atomic<u32>>;
@group(0) @binding(9) var<storage, read_write> scores: array<u32>;

const OP_LUMA: u32 = 1u;
const OP_DOWN_H: u32 = 2u;
const OP_DOWN_V: u32 = 3u;
const OP_TENSOR_H: u32 = 4u;
const OP_SUM_V: u32 = 5u;
const OP_MISMATCH_H: u32 = 6u;
const OP_UPDATE: u32 = 7u;
const OP_STILL_H: u32 = 8u;
const OP_STILL_V: u32 = 9u;
const OP_MEDIAN: u32 = 10u;
const OP_UPSAMPLE: u32 = 11u;
const OP_ZERO: u32 = 12u;
const OP_SPLAT_OUT: u32 = 15u;
const OP_FILL: u32 = 16u;
const OP_WARP: u32 = 17u;

// (the constants of `flow.rs`)
const WIN: i32 = 4;
const MAX_STEP: f32 = 2.0;
const FB_WEIGHT: f32 = 0.05;
const AGREE_PX: f32 = 1.0;
const STILL_LUMA: f32 = 0.004;
const NONE: u32 = 0xffffffffu;

fn finite(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u;
}

fn dims(t: texture_2d<f32>) -> vec2<i32> {
    return vec2<i32>(textureDimensions(t));
}

// `g_at` / `Flow::at`: clamped fetch
fn at(t: texture_2d<f32>, x: i32, y: i32) -> vec4<f32> {
    let s = dims(t);
    return textureLoad(t, clamp(vec2(x, y), vec2(0), s - 1), 0);
}

// `g_sample` / `Flow::sample`: bilinear, pixel centres at integers, clamped; non-finite reads 0
fn samp(t: texture_2d<f32>, x_in: f32, y_in: f32) -> vec4<f32> {
    let x = select(0.0, x_in, finite(x_in));
    let y = select(0.0, y_in, finite(y_in));
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let i = i32(x0);
    let j = i32(y0);
    let a = at(t, i, j);
    let b = at(t, i + 1, j);
    let c = at(t, i, j + 1);
    let d = at(t, i + 1, j + 1);
    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    return top + (bot - top) * fy;
}

// `flow::luma` of a premultiplied linear pixel
fn luma(p: vec3<f32>) -> f32 {
    let l = 0.2126 * p.x + 0.7152 * p.y + 0.0722 * p.z;
    if !finite(l) {
        return 0.0;
    }
    return sqrt(clamp(l, 0.0, 16.0));
}

// the gradient of luma image t0 at (x, y), as `refine`
fn grad(x: i32, y: i32) -> vec2<f32> {
    return vec2((at(t0, x + 1, y).r - at(t0, x - 1, y).r) * 0.5, (at(t0, x, y + 1).r - at(t0, x, y - 1).r) * 0.5);
}

// `Image::sample_bilinear_clamped` of a full-size image (pixel centres at +0.5)
fn sample_img(t: texture_2d<f32>, x: f32, y: f32) -> vec4<f32> {
    let s = dims(t);
    let fx = clamp(x - 0.5, 0.0, f32(s.x) - 1.0);
    let fy = clamp(y - 0.5, 0.0, f32(s.y) - 1.0);
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, s.x - 1);
    let y1 = min(y0 + 1, s.y - 1);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let a = textureLoad(t, vec2(x0, y0), 0);
    let b = textureLoad(t, vec2(x1, y0), 0);
    let c = textureLoad(t, vec2(x0, y1), 0);
    let d = textureLoad(t, vec2(x1, y1), 0);
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    return top + (bot - top) * ty;
}

fn agree(d: f32) -> f32 {
    let q = d / AGREE_PX;
    return 1.0 / (1.0 + q * q);
}

// compare-exchange of the median network (each component on its own)
fn lo(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> { return min(a, b); }
fn hi(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> { return max(a, b); }

fn median9(p: vec2<i32>) -> vec2<f32> {
    var p0 = at(t2, p.x - 1, p.y - 1).rg; var p1 = at(t2, p.x, p.y - 1).rg; var p2 = at(t2, p.x + 1, p.y - 1).rg;
    var p3 = at(t2, p.x - 1, p.y).rg;     var p4 = at(t2, p.x, p.y).rg;     var p5 = at(t2, p.x + 1, p.y).rg;
    var p6 = at(t2, p.x - 1, p.y + 1).rg; var p7 = at(t2, p.x, p.y + 1).rg; var p8 = at(t2, p.x + 1, p.y + 1).rg;
    var t: vec2<f32>;
    t = lo(p1, p2); p2 = hi(p1, p2); p1 = t;
    t = lo(p4, p5); p5 = hi(p4, p5); p4 = t;
    t = lo(p7, p8); p8 = hi(p7, p8); p7 = t;
    t = lo(p0, p1); p1 = hi(p0, p1); p0 = t;
    t = lo(p3, p4); p4 = hi(p3, p4); p3 = t;
    t = lo(p6, p7); p7 = hi(p6, p7); p6 = t;
    t = lo(p1, p2); p2 = hi(p1, p2); p1 = t;
    t = lo(p4, p5); p5 = hi(p4, p5); p4 = t;
    t = lo(p7, p8); p8 = hi(p7, p8); p7 = t;
    p3 = hi(p0, p3);
    p5 = lo(p5, p8);
    t = lo(p4, p7); p7 = hi(p4, p7); p4 = t;
    p6 = hi(p3, p6);
    p4 = hi(p1, p4);
    p2 = lo(p2, p5);
    p4 = lo(p4, p7);
    t = lo(p4, p2); p2 = hi(p4, p2); p4 = t;
    p4 = hi(p6, p4);
    return lo(p4, p2);
}

fn pixel(op: u32, p: vec2<i32>) -> vec4<f32> {
    switch op {
        case OP_LUMA: {
            // `work_luma`: the mean luma of each s×s block of the working image t0
            let s = i32(u.i1.x);
            let full = dims(t0);
            var sum = 0.0;
            var n = 0u;
            for (var yy = p.y * s; yy < min((p.y + 1) * s, full.y); yy++) {
                for (var xx = p.x * s; xx < min((p.x + 1) * s, full.x); xx++) {
                    sum += luma(textureLoad(t0, vec2(xx, yy), 0).rgb);
                    n += 1u;
                }
            }
            return vec4(select(0.0, sum / f32(n), n > 0u), 0.0, 0.0, 0.0);
        }
        case OP_DOWN_H: {
            // `Gray::down`: the 5-tap binomial along x
            var acc = 0.0;
            acc += 0.0625 * at(t0, p.x - 2, p.y).r;
            acc += 0.25 * at(t0, p.x - 1, p.y).r;
            acc += 0.375 * at(t0, p.x, p.y).r;
            acc += 0.25 * at(t0, p.x + 1, p.y).r;
            acc += 0.0625 * at(t0, p.x + 2, p.y).r;
            return vec4(acc, 0.0, 0.0, 0.0);
        }
        case OP_DOWN_V: {
            // then along y, every second sample
            let x = 2 * p.x; let y = 2 * p.y;
            var acc = 0.0;
            acc += 0.0625 * at(t0, x, y - 2).r;
            acc += 0.25 * at(t0, x, y - 1).r;
            acc += 0.375 * at(t0, x, y).r;
            acc += 0.25 * at(t0, x, y + 1).r;
            acc += 0.0625 * at(t0, x, y + 2).r;
            return vec4(acc, 0.0, 0.0, 0.0);
        }
        case OP_TENSOR_H: {
            // the gradient products of luma t0, summed along x over the window
            let w = dims(t0).x;
            var acc = vec3(0.0);
            for (var x = max(p.x - WIN, 0); x < min(p.x + WIN + 1, w); x++) {
                let g = grad(x, p.y);
                acc += vec3(g.x * g.x, g.x * g.y, g.y * g.y);
            }
            return vec4(acc, 0.0);
        }
        case OP_SUM_V: {
            // t0 summed along y over the window
            let h = dims(t0).y;
            var acc = vec4(0.0);
            for (var y = max(p.y - WIN, 0); y < min(p.y + WIN + 1, h); y++) {
                acc += textureLoad(t0, vec2(p.x, y), 0);
            }
            return acc;
        }
        case OP_MISMATCH_H: {
            // gradient × the warped luma error (a = t0, b = t1, flow t2), summed along x
            let w = dims(t0).x;
            var acc = vec2(0.0);
            for (var x = max(p.x - WIN, 0); x < min(p.x + WIN + 1, w); x++) {
                let f = textureLoad(t2, vec2(x, p.y), 0).rg;
                let e = samp(t1, f32(x) + f.x, f32(p.y) + f.y).r - textureLoad(t0, vec2(x, p.y), 0).r;
                acc += grad(x, p.y) * e;
            }
            return vec4(acc, 0.0, 0.0);
        }
        case OP_UPDATE: {
            // one Gauss–Newton step: the mismatch sums t0 summed along y, the tensor t1, flow t2
            let h = dims(t0).y;
            var bv = vec2(0.0);
            for (var y = max(p.y - WIN, 0); y < min(p.y + WIN + 1, h); y++) {
                bv += textureLoad(t0, vec2(p.x, y), 0).rg;
            }
            let g = textureLoad(t1, p, 0);
            var f = textureLoad(t2, p, 0).rg;
            let damp = u.p0.z;
            let gxx = g.x + damp; let gxy = g.y; let gyy = g.z + damp;
            let det = gxx * gyy - gxy * gxy;
            if det > 1e-12 {
                let dx = -(gyy * bv.x - gxy * bv.y) / det;
                let dy = -(gxx * bv.y - gxy * bv.x) / det;
                if finite(dx) && finite(dy) {
                    f += vec2(clamp(dx, -MAX_STEP, MAX_STEP), clamp(dy, -MAX_STEP, MAX_STEP));
                }
            }
            return vec4(f, 0.0, 0.0);
        }
        case OP_STILL_H: {
            // `prefer_still`: the squared errors of moving and of standing still, summed along x
            let w = dims(t0).x;
            var acc = vec2(0.0);
            for (var x = max(p.x - WIN, 0); x < min(p.x + WIN + 1, w); x++) {
                let q = vec2(x, p.y);
                let f = textureLoad(t2, q, 0).rg;
                let a = textureLoad(t0, q, 0).r;
                let moved = samp(t1, f32(x) + f.x, f32(p.y) + f.y).r - a;
                let still = textureLoad(t1, q, 0).r - a;
                acc += vec2(moved * moved, still * still);
            }
            return vec4(acc, 0.0, 0.0);
        }
        case OP_STILL_V: {
            let h = dims(t0).y;
            var e = vec2(0.0);
            for (var y = max(p.y - WIN, 0); y < min(p.y + WIN + 1, h); y++) {
                e += textureLoad(t0, vec2(p.x, y), 0).rg;
            }
            if e.y <= e.x + u.p0.w {
                return vec4(0.0);
            }
            return vec4(textureLoad(t2, p, 0).rg, 0.0, 0.0);
        }
        case OP_MEDIAN: {
            return vec4(median9(p), 0.0, 0.0);
        }
        case OP_UPSAMPLE: {
            // `upsample`: the coarser flow t2 on this grid, its vectors doubled
            let v = samp(t2, f32(p.x) * 0.5, f32(p.y) * 0.5).rg;
            return vec4(v * 2.0, 0.0, 0.0);
        }
        case OP_SPLAT_OUT: {
            // the winning splat's flow (frame 0's u01 or frame 1's −u10), .b = 1 when one landed
            let w = u.i0.y;
            let n = w * u.i0.z;
            let j = u32(p.y) * w + u32(p.x);
            let won = atomicLoad(&winner[j]);
            if won == 0u {
                return vec4(0.0);
            }
            let order = ~won;
            let i = order % n;
            let q = vec2(i32(i % w), i32(i / w));
            if order < n {
                return vec4(textureLoad(t2, q, 0).rg, 1.0, 0.0);
            }
            return vec4(-textureLoad(t3, q, 0).rg, 1.0, 0.0);
        }
        case OP_FILL: {
            // one hole-filling pass: a hole takes the mean of its filled neighbours
            let o = textureLoad(t0, p, 0);
            if o.b != 0.0 {
                return o;
            }
            let s = dims(t0);
            var sum = vec2(0.0);
            var k = 0u;
            for (var dy = -1; dy <= 1; dy++) {
                for (var dx = -1; dx <= 1; dx++) {
                    let q = p + vec2(dx, dy);
                    if (dx != 0 || dy != 0) && q.x >= 0 && q.y >= 0 && q.x < s.x && q.y < s.y {
                        let v = textureLoad(t0, q, 0);
                        if v.b != 0.0 {
                            sum += v.rg;
                            k += 1u;
                        }
                    }
                }
            }
            if k > 0u {
                return vec4(sum / f32(k), 1.0, 0.0);
            }
            return o;
        }
        case OP_WARP: {
            // `interpolate`'s last step: frame 0 (t0) and frame 1 (t1) fetched along the
            // in-between flow (t2), weighted by how well the fetched pixels' own flows (t3 = u01,
            // t4 = u10) agree, faded to the cross-fade where nothing changed
            let t = u.p0.x;
            let sf = u.p0.y;
            let pc = vec2<f32>(p) + 0.5;
            let uu = samp(t2, pc.x / sf - 0.5, pc.y / sf - 0.5).rg;
            let ux = uu * sf;
            let q0 = pc - t * ux;
            let q1 = pc + (1.0 - t) * ux;
            let ua = samp(t3, q0.x / sf - 0.5, q0.y / sf - 0.5).rg;
            let ub = samp(t4, q1.x / sf - 0.5, q1.y / sf - 0.5).rg;
            let ka = agree(length(ua - uu));
            let kb = agree(length(ub + uu));
            let wa = (1.0 - t) * ka;
            let wb = t * kb;
            let ca = sample_img(t0, q0.x, q0.y);
            let cb = sample_img(t1, q1.x, q1.y);
            let pa = textureLoad(t0, p, 0);
            let pb = textureLoad(t1, p, 0);
            let sum = wa + wb;
            let ch = max(abs(luma(pa.rgb) - luma(pb.rgb)), abs(pa.a - pb.a));
            let changed = select(0.0, clamp((ch - STILL_LUMA) / STILL_LUMA, 0.0, 1.0), finite(ch));
            let conf = max(ka, kb) * changed;
            var warped = ca * (1.0 - t) + cb * t;
            if sum > 1e-6 {
                warped = (ca * wa + cb * wb) / sum;
            }
            let plain = pa + (pb - pa) * t;
            return plain + (warped - plain) * conf;
        }
        default: {
            // OP_ZERO
            return vec4(0.0);
        }
    }
}

@compute @workgroup_size(16, 16)
fn flow_px(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= u.i0.y || id.y >= u.i0.z {
        return;
    }
    let p = vec2<i32>(id.xy);
    textureStore(out, p, pixel(u.i0.x, p));
}

// ---- the splat (`flow_at`): one invocation per source pixel, frame 0 in rows 0..h, frame 1 in
// h..2h; t0 / t1 the luma of frame 0 / 1, t2 / t3 the flows u01 / u10. The CPU visits frame 0's
// pixels then frame 1's and keeps the first lowest score: here the lowest score wins, and among
// equal scores the source visited first.

fn source(id: vec3<u32>) -> vec2<u32> {
    // (order of the source pixel, 1 when it is valid)
    let w = u.i0.y;
    let h = u.i0.z;
    if id.x >= w || id.y >= 2u * h {
        return vec2(0u, 0u);
    }
    return vec2(id.y * w + id.x, 1u);
}

@compute @workgroup_size(16, 16)
fn splat_score(@builtin(global_invocation_id) id: vec3<u32>) {
    let s = source(id);
    if s.y == 0u {
        return;
    }
    let w = i32(u.i0.y);
    let h = i32(u.i0.z);
    let t = u.p0.x;
    let second = i32(id.y) >= h;
    let p = vec2(i32(id.x), i32(id.y) - select(0, h, second));
    var su: f32; var dst: f32; var uv: vec2<f32>; var back: vec2<f32>; var tt: f32;
    if second {
        uv = textureLoad(t3, p, 0).rg;
        let q = vec2<f32>(p) + uv;
        su = textureLoad(t1, p, 0).r;
        dst = samp(t0, q.x, q.y).r;
        back = samp(t2, q.x, q.y).rg;
        tt = 1.0 - t;
    } else {
        uv = textureLoad(t2, p, 0).rg;
        let q = vec2<f32>(p) + uv;
        su = textureLoad(t0, p, 0).r;
        dst = samp(t1, q.x, q.y).r;
        back = samp(t3, q.x, q.y).rg;
        tt = t;
    }
    let photo = abs(su - dst);
    let fb = min(length(uv + back), 100.0);
    let score = photo + FB_WEIGHT * fb;
    let k = s.x * 3u;
    scores[k] = NONE;
    let tp = vec2<f32>(p) + tt * uv;
    if !finite(score) || !(tp.x > -1.0 && tp.y > -1.0 && tp.x < f32(w) && tp.y < f32(h)) {
        return;
    }
    let bits = bitcast<u32>(score);
    let c = vec2<i32>(floor(tp));
    scores[k] = bits;
    scores[k + 1u] = bitcast<u32>(c.x);
    scores[k + 2u] = bitcast<u32>(c.y);
    for (var n = 0; n < 4; n++) {
        let q = c + vec2(n % 2, n / 2);
        if q.x >= 0 && q.y >= 0 && q.x < w && q.y < h {
            atomicMax(&best[u32(q.y * w + q.x)], ~bits);
        }
    }
}

@compute @workgroup_size(16, 16)
fn splat_win(@builtin(global_invocation_id) id: vec3<u32>) {
    let s = source(id);
    if s.y == 0u {
        return;
    }
    let w = i32(u.i0.y);
    let h = i32(u.i0.z);
    let k = s.x * 3u;
    let bits = scores[k];
    if bits == NONE {
        return;
    }
    let c = vec2(bitcast<i32>(scores[k + 1u]), bitcast<i32>(scores[k + 2u]));
    for (var n = 0; n < 4; n++) {
        let q = c + vec2(n % 2, n / 2);
        if q.x >= 0 && q.y >= 0 && q.x < w && q.y < h {
            let j = u32(q.y * w + q.x);
            if atomicLoad(&best[j]) == ~bits {
                atomicMax(&winner[j], ~s.x);
            }
        }
    }
}

struct Params {
    scene: u32,
    width: u32,
    height: u32,
    time_bits: u32,
};
struct U64Pair {
    lo: u32,
    hi: u32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var output_image: texture_storage_2d<rgba8unorm, write>;

fn xor64(a: U64Pair, b: U64Pair) -> U64Pair {
    return U64Pair(a.lo ^ b.lo, a.hi ^ b.hi);
}
fn shr64(a: U64Pair, shift: u32) -> U64Pair {
    if (shift == 0u) { return a; }
    if (shift < 32u) {
        return U64Pair((a.lo >> shift) | (a.hi << (32u - shift)), a.hi >> shift);
    }
    if (shift < 64u) { return U64Pair(a.hi >> (shift - 32u), 0u); }
    return U64Pair(0u, 0u);
}
fn mul64(a: U64Pair, b: U64Pair) -> U64Pair {
    let a0 = a.lo & 0xffffu;
    let a1 = a.lo >> 16u;
    let b0 = b.lo & 0xffffu;
    let b1 = b.lo >> 16u;
    let p0 = a0 * b0;
    let p1 = a0 * b1;
    let p2 = a1 * b0;
    let p3 = a1 * b1;
    let middle = (p0 >> 16u) + (p1 & 0xffffu) + (p2 & 0xffffu);
    let low = (p0 & 0xffffu) | (middle << 16u);
    let high = a.hi * b.lo + a.lo * b.hi + p3 + (p1 >> 16u) + (p2 >> 16u) + (middle >> 16u);
    return U64Pair(low, high);
}
fn signed_pair(n: i32) -> U64Pair {
    return U64Pair(bitcast<u32>(n), select(0u, 0xffffffffu, n < 0));
}
fn hash64(n: U64Pair) -> f32 {
    var x = xor64(n, U64Pair(0x7f4a7c15u, 0x9e3779b9u));
    x = mul64(xor64(x, shr64(x, 30u)), U64Pair(0x1ce4e5b9u, 0xbf58476du));
    x = mul64(xor64(x, shr64(x, 27u)), U64Pair(0x133111ebu, 0x94d049bbu));
    x = xor64(x, shr64(x, 31u));
    let top24 = x.hi >> 8u;
    return f32(top24) / 16777216.0 * 2.0 - 1.0;
}
fn hash_i32(n: i32) -> f32 { return hash64(signed_pair(n)); }
fn hash2_i32(a: i32, b: i32) -> f32 {
    let ah = mul64(signed_pair(a), U64Pair(73856093u, 0u));
    let bh = mul64(signed_pair(b), U64Pair(19349663u, 0u));
    return hash64(xor64(ah, bh));
}
fn lerp3(a: vec3<f32>, b: vec3<f32>, t: f32) -> vec3<f32> {
    return a + (b - a) * t;
}
fn smoothstep_cpu(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn div_corrected(a: f32, b: f32) -> f32 {
    // Positive normal coordinates only. Match Rust's nearest-even division before discrete floors.
    let ai = bitcast<u32>(a);
    let bi = bitcast<u32>(b);
    if (ai == 0u) { return 0.0; }
    let ma = (ai & 0x7fffffu) | 0x800000u;
    let mb = (bi & 0x7fffffu) | 0x800000u;
    var exponent = i32(ai >> 23u) - i32(bi >> 23u) + 127;
    var remainder = ma;
    if (remainder < mb) {
        remainder <<= 1u;
        exponent -= 1;
    }
    remainder -= mb;
    var mantissa = 0x800000u;
    for (var bit = 22; bit >= 0; bit -= 1) {
        remainder <<= 1u;
        if (remainder >= mb) {
            mantissa |= 1u << u32(bit);
            remainder -= mb;
        }
    }
    let twice = remainder << 1u;
    if (twice > mb || (twice == mb && (mantissa & 1u) != 0u)) { mantissa += 1u; }
    if (mantissa == 0x1000000u) { mantissa >>= 1u; exponent += 1; }
    return bitcast<f32>((u32(exponent) << 23u) | (mantissa & 0x7fffffu));
}
fn finite32(value: f32) -> f32 {
    // Observing each result's bits preserves Rust's f32 steps at discrete window/trail boundaries.
    let bits = bitcast<u32>(value);
    return bitcast<f32>(select(bits, 0u, (bits & 0x7f800000u) == 0x7f800000u));
}
fn vnoise(x: f32, y: f32) -> f32 {
    let xi = floor(x);
    let yi = floor(y);
    let xf = x - xi;
    let yf = y - yi;
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let i = i32(xi);
    let j = i32(yi);
    let a = hash2_i32(i, j);
    let b = hash2_i32(i + 1, j);
    let c = hash2_i32(i, j + 1);
    let d = hash2_i32(i + 1, j + 1);
    let top = a + (b - a) * u;
    let bottom = c + (d - c) * u;
    return top + (bottom - top) * v;
}
fn fbm(x: f32, y: f32, octaves: u32) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var fx = x;
    var fy = y;
    for (var i = 0u; i < octaves; i += 1u) {
        sum += amplitude * vnoise(fx, fy);
        fx = fx * 2.03 + 1.7;
        fy = fy * 2.01 + 9.2;
        amplitude *= 0.5;
    }
    return sum;
}
fn ocean(u: f32, v: f32, aspect: f32, t: f32) -> vec3<f32> {
    let horizon = 0.58;
    let sun = vec2(0.5 + 0.02 * sin(t * 0.05), 0.42 + t * 0.006);
    if (v < horizon) {
        let k = v / horizon;
        var c = lerp3(vec3(0.16, 0.10, 0.32), vec3(0.98, 0.52, 0.28), pow(k, 1.6));
        c = lerp3(c, vec3(1.0, 0.82, 0.55), smoothstep_cpu(0.75, 1.0, k) * 0.6);
        let dx = (u - sun.x) * aspect;
        let dy = v - sun.y;
        let d = sqrt(dx * dx + dy * dy);
        let glow = exp(-d * 9.0);
        c = lerp3(c, vec3(1.0, 0.93, 0.75), glow * 0.8);
        if (d < 0.055) { c = vec3(1.0, 0.95, 0.82); }
        let clouds = fbm(u * 4.0 + t * 0.02, v * 14.0, 4u);
        let mask = smoothstep_cpu(0.15, 0.45, clouds) * smoothstep_cpu(0.1, 0.45, v) * (1.0 - smoothstep_cpu(0.45, 0.55, v));
        return lerp3(c, vec3(0.55, 0.28, 0.35), mask * 0.55);
    }
    let k = (v - horizon) / (1.0 - horizon);
    var c = lerp3(vec3(0.35, 0.18, 0.28), vec3(0.03, 0.05, 0.12), pow(k, 0.7));
    let wave = fbm(u * 18.0 * (1.0 - k * 0.6) + t * 0.3, (v - horizon) * 90.0 / (0.3 + k) - t * 1.4, 3u);
    let dx = (u - sun.x) * aspect;
    let path = exp(-dx * dx * 160.0 / (0.2 + k * 2.0));
    let sparkle = smoothstep_cpu(0.05, 0.4, wave) * path;
    c = lerp3(c, vec3(1.0, 0.78, 0.5), sparkle * (1.0 - k * 0.5));
    return lerp3(c, vec3(0.9, 0.5, 0.35), path * 0.25 * (1.0 - k));
}
fn aurora(u: f32, v: f32, t: f32, sx: u32, sy: u32) -> vec3<f32> {
    var c = lerp3(vec3(0.01, 0.02, 0.08), vec3(0.02, 0.1, 0.18), v);
    let st = hash_i32(i32(sx) * 7919 + i32(sy) * 104729);
    if (st > 0.994 && v < 0.7) {
        let twinkle = 0.6 + 0.4 * sin(t * 3.0 + st * 100.0);
        c = lerp3(c, vec3(0.9, 0.95, 1.0), twinkle);
    }
    for (var i = 0u; i < 3u; i += 1u) {
        let fi = f32(i);
        var color = vec3(0.2, 1.0, 0.55);
        if (i == 1u) { color = vec3(0.55, 0.3, 1.0); }
        if (i == 2u) { color = vec3(0.1, 0.9, 0.8); }
        let base = 0.28 + 0.08 * fi + 0.06 * sin(u * 3.0 + t * 0.15 + fi) + 0.05 * fbm(u * 3.0 + t * 0.05, fi, 3u);
        let dy = v - base;
        let ray = 0.5 + 0.5 * fbm(u * 40.0 + t * 0.4 + fi * 10.0, fi, 2u);
        let falloff = select(exp(dy * 12.0), 1.0, dy > 0.0);
        let a = exp(-dy * dy * 90.0) * falloff * ray;
        c = lerp3(c, color, min(a * 0.8, 1.0));
    }
    let mountain = 0.72 + 0.08 * fbm(u * 3.0, 3.3, 5u) + 0.05 * sin(u * 5.0);
    if (v > mountain) {
        let snow = smoothstep_cpu(0.02, 0.0, v - mountain) * 0.3;
        c = lerp3(vec3(0.02, 0.03, 0.05), vec3(0.4, 0.5, 0.6), snow);
    }
    if (v > 0.9) { c = lerp3(c, vec3(0.05, 0.25, 0.2), 0.3 * (1.0 + sin(v * 400.0 + t * 4.0)) * 0.5); }
    return c;
}
fn city(u: f32, v: f32, aspect: f32, t: f32) -> vec3<f32> {
    var c = lerp3(vec3(0.05, 0.02, 0.15), vec3(0.55, 0.2, 0.35), pow(v, 2.2));
    for (var li = 0u; li < 3u; li += 1u) {
        var bw = 0.15; var base = 0.55; var col = vec3(0.12, 0.08, 0.2); var speed = 0.02;
        if (li == 1u) { bw = 0.28; base = 0.62; col = vec3(0.07, 0.05, 0.12); speed = 0.05; }
        if (li == 2u) { bw = 0.45; base = 0.7; col = vec3(0.03, 0.02, 0.06); speed = 0.12; }
        let x = finite32(div_corrected(u * aspect, bw) + finite32(t * speed));
        let building = floor(x);
        let hgt = base - 0.32 * (0.5 + 0.5 * hash_i32(i32(building) * 31 + i32(li)));
        if (v > hgt) {
            c = col;
            let xf = fract(x);
            let wx = floor(xf * 6.0);
            let wy = floor((v - hgt) * 70.0);
            let lit = hash_i32(i32(building) * 1000 + i32(wx) * 37 + i32(wy) * 101 + i32(li));
            if (fract(xf * 6.0) < 0.55 && fract((v - hgt) * 70.0) < 0.5 && lit > 0.35 && xf > 0.08 && xf < 0.92) {
                let warm = vec3(1.0, 0.8 + 0.1 * lit, 0.45);
                c = lerp3(c, warm, 0.35 + 0.35 * (f32(li) / 2.0));
            }
        }
    }
    if (v > 0.86) {
        c = vec3(0.04, 0.03, 0.06);
        for (var k = 0u; k < 6u; k += 1u) {
            let fk = f32(k);
            let lane = 0.88 + fk * 0.02;
            let d = abs(v - lane);
            let horizontal = finite32(u * 6.0);
            let factor = finite32(1.5 + finite32(fk * 0.4));
            let elapsed = finite32(t * factor);
            let streak_phase = finite32(finite32(horizontal - elapsed) + fk);
            let streak = streak_phase - trunc(streak_phase);
            var color = vec3(1.0, 0.25, 0.2);
            if (k % 2u != 0u) { color = vec3(1.0, 0.9, 0.7); }
            c = lerp3(c, color, exp(-d * 900.0) * smoothstep_cpu(0.0, 0.8, streak));
        }
    }
    return c;
}
fn dunes(u: f32, v: f32, aspect: f32, t: f32) -> vec3<f32> {
    var c = lerp3(vec3(0.35, 0.6, 0.9), vec3(0.98, 0.85, 0.65), smoothstep_cpu(0.0, 0.5, v));
    let sx = (u - 0.75) * aspect;
    let sun_d = sqrt(sx * sx + (v - 0.22) * (v - 0.22));
    c = lerp3(c, vec3(1.0, 0.97, 0.85), exp(-sun_d * 14.0));
    for (var k = 0u; k < 4u; k += 1u) {
        let fk = f32(k);
        let x = u * (1.0 + fk * 0.6) + t * 0.01 * (fk + 1.0);
        let ridge = 0.45 + fk * 0.12 + 0.06 * sin(x * 6.0 + fk) + 0.03 * fbm(x * 4.0, fk, 3u);
        if (v > ridge) {
            let shade = 0.5 + 0.5 * cos(x * 6.0 + fk);
            let base = lerp3(vec3(0.78, 0.45, 0.22), vec3(0.98, 0.72, 0.45), shade);
            c = lerp3(base, vec3(0.55, 0.28, 0.15), min(v - ridge, 0.2) * 2.0 * (fk / 3.0));
        }
    }
    return c;
}
fn plasma(u: f32, v: f32, aspect: f32, t: f32) -> vec3<f32> {
    let x = u * aspect * 3.0;
    let y = v * 3.0;
    let p = sin(x + t) + sin(y * 1.3 - t * 0.7) + sin(x + y + t * 0.5) + sin(sqrt(x * x + y * y) * 2.0 - t * 1.2);
    let h = p * 0.25 + t * 0.05;
    return vec3(0.5 + 0.5 * sin(3.141592653589793 * h), 0.5 + 0.5 * sin(3.141592653589793 * h + 2.1) * 0.6, 0.5 + 0.5 * sin(3.141592653589793 * h + 4.2));
}
fn forest(u: f32, v: f32, t: f32) -> vec3<f32> {
    var c = lerp3(vec3(0.75, 0.82, 0.8), vec3(0.35, 0.45, 0.4), v);
    let ray = 0.5 + 0.5 * fbm(u * 8.0 - v * 3.0 + t * 0.05, 1.0, 2u);
    c = lerp3(c, vec3(1.0, 0.97, 0.85), ray * (1.0 - v) * 0.35);
    for (var k = 0u; k < 5u; k += 1u) {
        let fk = f32(k);
        let depth = 1.0 - fk / 5.0;
        let spacing = 0.12 + fk * 0.03;
        let x = u / spacing + t * 0.03 * (fk + 1.0) + fk * 3.3;
        let trunk_x = fract(x);
        let tw = 0.08 + 0.05 * (1.0 - depth);
        let id = floor(x);
        if (abs(trunk_x - 0.5) < tw && hash_i32(i32(id) * 13 + i32(k)) > -0.2) {
            let dark = lerp3(vec3(0.45, 0.5, 0.48), vec3(0.05, 0.07, 0.05), 1.0 - depth * 0.8);
            c = lerp3(c, dark, 0.85);
        }
    }
    let mist = smoothstep_cpu(0.55, 1.0, v) * (0.6 + 0.4 * fbm(u * 3.0 + t * 0.1, v * 5.0, 3u));
    return lerp3(c, vec3(0.85, 0.88, 0.86), mist * 0.6);
}
fn pixel(scene: u32, u: f32, v: f32, aspect: f32, t: f32, sx: u32, sy: u32) -> vec3<f32> {
    if (scene == 0u) { return aurora(u, v, t, sx, sy); }
    if (scene == 1u) { return ocean(u, v, aspect, t); }
    if (scene == 2u) { return city(u, v, aspect, t); }
    if (scene == 3u) { return dunes(u, v, aspect, t); }
    if (scene == 4u) { return plasma(u, v, aspect, t); }
    return forest(u, v, t);
}
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let t = bitcast<f32>(params.time_bits);
    let u = div_corrected(f32(gid.x), f32(params.width));
    let v = div_corrected(f32(gid.y), f32(params.height));
    let aspect = div_corrected(f32(params.width), f32(params.height));
    let sx = gid.x * 900u / params.width;
    let sy = gid.y * 500u / params.height;
    let c = pixel(params.scene, u, v, aspect, t, sx, sy);
    // CPU `to8` truncates after multiplying by 255; quantize before the UNORM store.
    let rgba = vec4(floor(clamp(c, vec3(0.0), vec3(1.0)) * 255.0) / 255.0, 1.0);
    textureStore(output_image, vec2<i32>(gid.xy), rgba);
}

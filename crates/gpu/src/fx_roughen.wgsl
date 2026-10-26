// Roughen Edges as a three-pass plan (crates/render/src/gpufx_roughen.rs), the GPU form of
// `roughen_edges` in crates/render/src/vfx/stylize.rs.
//
// CONCATENATION: this file uses U64, mulhi32, u64_of_i32, xor64, shr64, mul64 and hash_u64 from
// `noise_u64.wgsl`. WGSL has no include, so the integrator prepends `noise_u64.wgsl` to this file
// before building the module.
//
// Bindings (kernel contract of fx_multipass.wgsl, plus one uniform):
//   0: input0  (texture_2d<f32>, unfilterable, textureLoad)
//   1: input1  (texture_2d<f32>, unfilterable, textureLoad)
//   2: output  (texture_storage_2d<rgba32float, write>)
//   3: params  (uniform array<vec4<u32>, 8>: the pass parameter block, RoughenParams::uniform_bytes,
//               80 bytes zero-padded; read word by word, f32 words through bitcast)
//
// Passes (the CPU's three box passes per axis, in the CPU's order: all horizontal, then vertical):
//   cs_roughen_box x3 horizontal (alpha -> slot1 -> slot2 -> slot3), cs_roughen_box x3 vertical
//   (slot3 -> slot1 -> slot2 -> slot3), then cs_roughen_apply (slot0 + slot3 -> slot4).
// Each box pass reads its radius and axis from its own parameter block (words 0..2, see
// gpufx_roughen.rs). Zero outside the image, normalised by 2r+1, as box_rows does with repeat=false.

@group(0) @binding(0) var input0: texture_2d<f32>;
@group(0) @binding(1) var input1: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: array<vec4<u32>, 8>;

// Word layout of the parameter block (RoughenParams::uniform_bytes):
//   0 sigma, 1 sharpness, 2 influence, 3 scale (px), 4 stretch (2^x), 5 offset x, 6 offset y,
//   7 complexity, 8 evolution (turns), 9..11 unused, 12 seed lo, 13 seed hi, 14 edge type,
//   15 unused, 16..18 edge colour (linear RGB), 19 unused.
fn pword(i: u32) -> u32 {
    return params[i / 4u][i % 4u];
}

fn pf(i: u32) -> f32 {
    return bitcast<f32>(pword(i));
}


fn add64(a: U64, b: U64) -> U64 {
    let lo = a.lo + b.lo;
    var carry = 0u;
    if (lo < a.lo) {
        carry = 1u;
    }
    return U64(a.hi + b.hi + carry, lo);
}

// h mod 12 for a 64-bit value: 2^32 mod 12 = 4.
fn mod12(h: U64) -> u32 {
    return ((h.hi % 12u) * 4u + (h.lo % 12u)) % 12u;
}

fn fade(t: f32) -> f32 {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

// The 12 cube edge directions of effects' grad3 (same index order as the CPU match).
fn grad3(h: U64, x: f32, y: f32, z: f32) -> f32 {
    let m = mod12(h);
    var g = -y - z;
    if (m == 0u) { g = x + y; }
    else if (m == 1u) { g = -x + y; }
    else if (m == 2u) { g = x - y; }
    else if (m == 3u) { g = -x - y; }
    else if (m == 4u) { g = x + z; }
    else if (m == 5u) { g = -x + z; }
    else if (m == 6u) { g = x - z; }
    else if (m == 7u) { g = -x - z; }
    else if (m == 8u) { g = y + z; }
    else if (m == 9u) { g = -y + z; }
    else if (m == 10u) { g = y - z; }
    return g;
}

// 3-D gradient noise, about -1..1 (mirror of vfx::noise3).
fn noise3(x: f32, y: f32, z: f32, seed: U64) -> f32 {
    let fx0 = floor(x);
    let fy0 = floor(y);
    let fz0 = floor(z);
    let xf = x - fx0;
    let yf = y - fy0;
    let zf = z - fz0;
    let xi = i32(clamp(fx0, -2.0e9, 2.0e9));
    let yi = i32(clamp(fy0, -2.0e9, 2.0e9));
    let zi = i32(clamp(fz0, -2.0e9, 2.0e9));
    let u = fade(xf);
    let v = fade(yf);
    let w = fade(zf);
    let g000 = grad3(hash_u64(u64_of_i32(xi), u64_of_i32(yi), u64_of_i32(zi), seed), xf, yf, zf);
    let g100 = grad3(hash_u64(u64_of_i32(xi + 1), u64_of_i32(yi), u64_of_i32(zi), seed), xf - 1.0, yf, zf);
    let g010 = grad3(hash_u64(u64_of_i32(xi), u64_of_i32(yi + 1), u64_of_i32(zi), seed), xf, yf - 1.0, zf);
    let g110 = grad3(hash_u64(u64_of_i32(xi + 1), u64_of_i32(yi + 1), u64_of_i32(zi), seed), xf - 1.0, yf - 1.0, zf);
    let g001 = grad3(hash_u64(u64_of_i32(xi), u64_of_i32(yi), u64_of_i32(zi + 1), seed), xf, yf, zf - 1.0);
    let g101 = grad3(hash_u64(u64_of_i32(xi + 1), u64_of_i32(yi), u64_of_i32(zi + 1), seed), xf - 1.0, yf, zf - 1.0);
    let g011 = grad3(hash_u64(u64_of_i32(xi), u64_of_i32(yi + 1), u64_of_i32(zi + 1), seed), xf, yf - 1.0, zf - 1.0);
    let g111 = grad3(hash_u64(u64_of_i32(xi + 1), u64_of_i32(yi + 1), u64_of_i32(zi + 1), seed), xf - 1.0, yf - 1.0, zf - 1.0);
    let x00 = g000 + (g100 - g000) * u;
    let x10 = g010 + (g110 - g010) * u;
    let x01 = g001 + (g101 - g001) * u;
    let x11 = g011 + (g111 - g011) * u;
    let y0 = x00 + (x10 - x00) * v;
    let y1 = x01 + (x11 - x01) * v;
    return (y0 + (y1 - y0) * w) * 0.9;
}

// fBm with fractional octaves (mirror of vfx::fbm; octaves clamped to 1..10).
fn fbm(x: f32, y: f32, z: f32, octaves: f32, seed: U64) -> f32 {
    let n = clamp(octaves, 1.0, 10.0);
    var sum = 0.0;
    var amp = 1.0;
    var freq = 1.0;
    var norm = 0.0;
    for (var i: u32 = 0u; i < 10u; i = i + 1u) {
        let fi = f32(i);
        if (fi >= n) {
            break;
        }
        let k = min(n - fi, 1.0);
        sum = sum + noise3(x * freq, y * freq, z * freq + fi * 17.13, add64(seed, U64(0u, i))) * amp * k;
        norm = norm + amp * k;
        amp = amp * 0.5;
        freq = freq * 2.0;
    }
    return sum / max(norm, 1e-6);
}

// Linear -> sRGB, as effects::enc (negatives clamp to 0).
fn enc1(c: vec3<f32>) -> vec3<f32> {
    let v = max(c, vec3<f32>(0.0));
    let lo = v * 12.92;
    let hi = 1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, v <= vec3<f32>(0.0031308));
}

fn unpremul(p: vec4<f32>) -> vec3<f32> {
    if (p.a <= 1e-6) {
        return vec3<f32>(0.0);
    }
    return p.rgb / p.a;
}

// One box pass along an axis: mean of 2r+1 taps, zero outside the image (CPU box_rows, repeat=false).
// Words: 0 radius, 1 1 = read alpha (first pass) / 0 = read red (later passes), 2 0 = horizontal / 1 = vertical.
@compute @workgroup_size(8, 8, 1)
fn cs_roughen_box(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let dims = vec2<i32>(textureDimensions(input0));
    let r = i32(pword(0u));
    let use_alpha = pword(1u) != 0u;
    let dir = select(vec2<i32>(0, 1), vec2<i32>(1, 0), pword(2u) == 0u);
    let texel = vec2<i32>(id.xy);
    var acc = 0.0;
    for (var t: i32 = -r; t <= r; t = t + 1) {
        let q = texel + dir * t;
        if (q.x >= 0 && q.y >= 0 && q.x < dims.x && q.y < dims.y) {
            let px = textureLoad(input0, q, 0);
            acc = acc + select(px.r, px.a, use_alpha);
        }
    }
    let v = acc / f32(2 * r + 1);
    textureStore(output, texel, vec4<f32>(v, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(8, 8, 1)
fn cs_roughen_apply(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let src = textureLoad(input0, texel, 0);
    if (src.a <= 1e-6) {
        textureStore(output, texel, src);
        return;
    }
    let d = textureLoad(input1, texel, 0).r;
    let kind = pword(14u);
    let sharp = pf(1u);
    let infl = pf(2u);
    let scale = pf(3u);
    let stretch = pf(4u);
    let off = vec2<f32>(pf(5u), pf(6u));
    let cplx = pf(7u);
    let evo = pf(8u);
    let seed = U64(pword(13u), pword(12u));
    let edge_color = vec3<f32>(pf(16u), pf(17u), pf(18u));

    let px = vec2<f32>(f32(texel.x), f32(texel.y));
    let cu = (px.x + off.x) / (scale * stretch);
    let cv = (px.y + off.y) * stretch / scale;
    var n = fbm(cu, cv, evo, cplx, seed);
    if (kind == 3u) {
        n = 1.0 - 2.0 * abs(n);
    }
    var k = clamp((d - 0.5 + n * infl * 0.5) * (1.0 + sharp * 4.0) + 0.5, 0.0, 1.0);
    if (kind == 2u) {
        k = select(0.0, 1.0, k > 0.5);
    }
    if (kind >= 4u && kind <= 7u) {
        let hole = fbm(cu * 2.3 + 11.0, cv * 2.3, evo, cplx, add64(seed, U64(0u, 1u)));
        if (d < 0.97 && hole > 0.25) {
            k = k * (1.0 - smoothstep(0.25, 0.4, hole) * (1.0 - d));
        }
    }

    var rgb = src.rgb;
    let colored = kind == 1u || kind == 5u || kind == 7u;
    if (colored && d < 0.97) {
        let c = unpremul(src);
        let t = min((1.0 - d) * 1.5, 1.0);
        rgb = (c + (edge_color - c) * t) * src.a;
    }
    if (kind == 6u) {
        let e = enc1(unpremul(src));
        var l = 0.0;
        if (0.2126 * e.r + 0.7152 * e.g + 0.0722 * e.b > 0.5) {
            l = 1.0;
        }
        rgb = vec3<f32>(l) * src.a;
    }

    textureStore(output, texel, vec4<f32>(rgb * k, src.a * k));
}

// Measurement probe (not a route): the apply's intermediates per pixel, for localising a divergence.
// Writes r = fbm before the edge-type remap, g = depth, b = cu, a = cv. Same bindings as the apply.
@compute @workgroup_size(8, 8, 1)
fn cs_roughen_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let d = textureLoad(input1, texel, 0).r;
    let scale = pf(3u);
    let stretch = pf(4u);
    let off = vec2<f32>(pf(5u), pf(6u));
    let px = vec2<f32>(f32(texel.x), f32(texel.y));
    let cu = (px.x + off.x) / (scale * stretch);
    let cv = (px.y + off.y) * stretch / scale;
    let n = fbm(cu, cv, pf(8u), pf(7u), U64(pword(13u), pword(12u)));
    textureStore(output, texel, vec4<f32>(n, d, cu, cv));
}

// Measurement probe (not a route): the 64-bit cell hash of cell (x, y, 0) at each texel, split into four
// 16-bit parts (exact in f32): r = hi low, g = hi high, b = lo low, a = lo high. Same bindings as the apply.
@compute @workgroup_size(8, 8, 1)
fn cs_roughen_hash_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let h = hash_u64(u64_of_i32(texel.x), u64_of_i32(texel.y), u64_of_i32(0), U64(pword(13u), pword(12u)));
    textureStore(output, texel, vec4<f32>(f32(h.hi & 0xffffu), f32(h.hi >> 16u), f32(h.lo & 0xffffu), f32(h.lo >> 16u)));
}

// Measurement probe (not a route): octave k of the fbm at (0, 0), column x = k. Row 0: noise3 value.
// Row 1: grad3 of corners 000, 001, 010, 100. Row 2: corners 110, 101, 011, 111. Same bindings as the apply.
@compute @workgroup_size(8, 8, 1)
fn cs_roughen_octave_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let k = u32(texel.x);
    let s = add64(U64(pword(13u), pword(12u)), U64(0u, k));
    let z = f32(k) * 17.13;
    let zi = floor(z);
    let zf = z - zi;
    let zc = i32(zi);
    var v = vec4<f32>(0.0);
    if (texel.y == 0) {
        v = vec4<f32>(noise3(0.0, 0.0, z, s), 0.0, 0.0, 0.0);
    } else if (texel.y == 1) {
        v = vec4<f32>(
            grad3(hash_u64(u64_of_i32(0), u64_of_i32(0), u64_of_i32(zc), s), 0.0, 0.0, zf),
            grad3(hash_u64(u64_of_i32(0), u64_of_i32(0), u64_of_i32(zc + 1), s), 0.0, 0.0, zf - 1.0),
            grad3(hash_u64(u64_of_i32(0), u64_of_i32(1), u64_of_i32(zc), s), 0.0, -1.0, zf),
            grad3(hash_u64(u64_of_i32(1), u64_of_i32(0), u64_of_i32(zc), s), -1.0, 0.0, zf));
    } else if (texel.y == 3) {
        let hh = hash_u64(u64_of_i32(0), u64_of_i32(0), u64_of_i32(zc + 1), s);
        v = vec4<f32>(f32(hh.hi & 0xffffu), f32(hh.hi >> 16u), f32(hh.lo & 0xffffu), f32(hh.lo >> 16u));
    } else if (texel.y == 4) {
        let hh = hash_u64(u64_of_i32(0), u64_of_i32(0), u64_of_i32(zc + 1), s);
        v = vec4<f32>(f32(mod12(hh)), 0.0, 0.0, 0.0);
    } else {
        v = vec4<f32>(
            grad3(hash_u64(u64_of_i32(1), u64_of_i32(1), u64_of_i32(zc), s), -1.0, -1.0, zf),
            grad3(hash_u64(u64_of_i32(1), u64_of_i32(0), u64_of_i32(zc + 1), s), -1.0, 0.0, zf - 1.0),
            grad3(hash_u64(u64_of_i32(0), u64_of_i32(1), u64_of_i32(zc + 1), s), 0.0, -1.0, zf - 1.0),
            grad3(hash_u64(u64_of_i32(1), u64_of_i32(1), u64_of_i32(zc + 1), s), -1.0, -1.0, zf - 1.0));
    }
    textureStore(output, texel, v);
}

// Measurement probe (not a route): the cell hash for x and z in {-1, 0, 1, 2} (y = 0). Texel (x, y) holds the
// cell x = (x mod 4) - 1, z = (y mod 4) - 1, split into four 16-bit parts like cs_roughen_hash_probe.
@compute @workgroup_size(8, 8, 1)
fn cs_roughen_hash_z_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let ix = (texel.x % 4) - 1;
    let iz = (texel.y % 4) - 1;
    let h = hash_u64(u64_of_i32(ix), u64_of_i32(0), u64_of_i32(iz), U64(pword(13u), pword(12u)));
    textureStore(output, texel, vec4<f32>(f32(h.hi & 0xffffu), f32(h.hi >> 16u), f32(h.lo & 0xffffu), f32(h.lo >> 16u)));
}

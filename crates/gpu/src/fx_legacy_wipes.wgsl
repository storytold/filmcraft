// Two-word u64 arithmetic keeps the CPU hash's wrapping semantics on WGSL's u32-only integers.
fn legacy_mul32(a: u32, b: u32) -> vec2<u32> {
    let a0 = a & 65535u; let a1 = a >> 16u;
    let b0 = b & 65535u; let b1 = b >> 16u;
    let p0 = a0 * b0; let p1 = a0 * b1; let p2 = a1 * b0;
    let mid = p0 + (p1 << 16u);
    let low = mid + (p2 << 16u);
    let carry = select(0u, 1u, mid < p0) + select(0u, 1u, low < mid);
    return vec2<u32>(low, a1 * b1 + (p1 >> 16u) + (p2 >> 16u) + carry);
}
fn legacy_mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let p = legacy_mul32(a.x, b.x);
    return vec2<u32>(p.x, p.y + a.x * b.y + a.y * b.x);
}
fn legacy_shr64(a: vec2<u32>, n: u32) -> vec2<u32> {
    if n == 0u { return a; }
    if n < 32u { return vec2<u32>((a.x >> n) | (a.y << (32u - n)), a.y >> n); }
    if n < 64u { return vec2<u32>(a.y >> (n - 32u), 0u); }
    return vec2<u32>(0u);
}
fn legacy_hash01(x: u32, y: u32) -> f32 {
    var h = legacy_mul64(vec2<u32>(x, 0u), vec2<u32>(0x7F4A7C15u, 0x9E3779B9u));
    h = h ^ legacy_mul64(vec2<u32>(y, 0u), vec2<u32>(0x27D4EB4Fu, 0xC2B2AE3Du));
    h = h ^ vec2<u32>(0x93D69E8Du, 0x6119AA8Au); // seed 41 * hash constant
    h = h ^ legacy_shr64(h, 31u);
    h = legacy_mul64(h, vec2<u32>(0x1CE4E5B9u, 0xBF58476Du));
    h = h ^ legacy_shr64(h, 29u);
    h = legacy_mul64(h, vec2<u32>(0x133111EBu, 0x94D049BBu));
    h = h ^ legacy_shr64(h, 32u);
    return f32(legacy_shr64(h, 40u).x) / 16777216.0;
}
fn legacy_wipe_alpha(g: f32, c: f32, soft: f32) -> f32 {
    if soft <= 1e-4 { return select(0.0, 1.0, c <= 0.0 || g >= c); }
    return clamp((g - c * (1.0 + soft) + soft) / soft, 0.0, 1.0);
}
fn legacy_wipes_apply(p: vec2<i32>) -> vec4<f32> {
    let original = ld(p);
    let q = vec2<f32>(p) + vec2<f32>(0.5);
    let op = u.i0.x;
    var alpha = 1.0;
    if op == 108u {
        let c = u.p0.x;
        if c <= 0.0 { return original; }
        // Reciprocal block sizes (CPU packs 1/bw, 1/bh): f32 `/` is not correctly rounded on GPUs.
        let bx = u32(f32(p.x) * u.p1.x);
        let by = u32(f32(p.y) * u.p1.y);
        let r = legacy_hash01(bx, by);
        let feather = u.p0.w;
        if u.i1.x != 0u && feather > 0.0 {
            let t = clamp((r - (c - feather)) / max(2.0 * feather, 1e-6), 0.0, 1.0);
            alpha = t * t * (3.0 - 2.0 * t);
        } else { alpha = select(0.0, 1.0, r >= c); }
        if c >= 1.0 { alpha = 0.0; }
    } else if op == 107u {
        let c = u.p0.x;
        if c <= 0.0 { return original; }
        let g = (q.x * u.p0.y + q.y * u.p0.z - u.p0.w) / u.p1.x;
        alpha = legacy_wipe_alpha(g, c, u.p1.y);
    }
    return original * alpha;
}

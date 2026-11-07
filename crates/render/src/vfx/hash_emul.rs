//! CPU mirror of the WGSL u64 emulation of [`super::hash_u`].
//!
//! WGSL has no 64-bit integers, so the GPU port of the value-noise hash works on `(hi, lo)` pairs of
//! `u32`. The functions here use exactly the operations the shader uses (32-bit wrapping mul, xor,
//! shifts, a 16-bit-limb high multiply), so a bit-exact match with `hash_u` proves the GPU
//! arithmetic without a GPU. Test-only: the product code path stays the u64 reference.

/// A 64-bit value as two `u32` halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct U64 {
    pub hi: u32,
    pub lo: u32,
}

impl U64 {
    pub(crate) fn from_u64(v: u64) -> Self {
        U64 { hi: (v >> 32) as u32, lo: v as u32 }
    }

    pub(crate) fn to_u64(self) -> u64 {
        ((self.hi as u64) << 32) | self.lo as u64
    }

    /// Sign-extends an `i32` to 64 bits (the shader's `u64_of_i32`).
    pub(crate) fn from_i32(v: i32) -> Self {
        U64 { hi: if v < 0 { u32::MAX } else { 0 }, lo: v as u32 }
    }

    pub(crate) fn xor(self, o: U64) -> U64 {
        U64 { hi: self.hi ^ o.hi, lo: self.lo ^ o.lo }
    }

    /// Logical right shift for `0 < n <= 32` (the shader only uses 29, 31 and 32).
    pub(crate) fn shr(self, n: u32) -> U64 {
        if n >= 32 { U64 { hi: 0, lo: self.hi >> (n - 32) } } else { U64 { hi: self.hi >> n, lo: (self.lo >> n) | (self.hi << (32 - n)) } }
    }

    /// Wrapping 64-bit multiply, built from 32-bit products.
    pub(crate) fn mul(self, o: U64) -> U64 {
        let lo = self.lo.wrapping_mul(o.lo);
        let hi = mulhi32(self.lo, o.lo).wrapping_add(self.lo.wrapping_mul(o.hi)).wrapping_add(self.hi.wrapping_mul(o.lo));
        U64 { hi, lo }
    }
}

/// High 32 bits of the full 32×32 product, from 16-bit limbs (no 64-bit intermediate).
pub(crate) fn mulhi32(a: u32, b: u32) -> u32 {
    let (ah, al, bh, bl) = (a >> 16, a & 0xffff, b >> 16, b & 0xffff);
    let t = al.wrapping_mul(bl);
    let w1 = ah.wrapping_mul(bl).wrapping_add(t >> 16);
    let w2 = al.wrapping_mul(bh).wrapping_add(w1 & 0xffff);
    ah.wrapping_mul(bh).wrapping_add(w1 >> 16).wrapping_add(w2 >> 16)
}

/// The shader's `hash_u`: same constants and steps as [`super::hash_u`], on `U64` pairs.
pub(crate) fn hash_u_pair(x: i64, y: i64, z: i64, seed: u64) -> U64 {
    let c1 = U64::from_u64(0x9E37_79B9_7F4A_7C15);
    let c2 = U64::from_u64(0xC2B2_AE3D_27D4_EB4F);
    let c3 = U64::from_u64(0x1656_67B1_9E37_79F9);
    let c4 = U64::from_u64(0x27D4_EB2F_1656_67C5);
    let c5 = U64::from_u64(0xBF58_476D_1CE4_E5B9);
    let c6 = U64::from_u64(0x94D0_49BB_1331_11EB);
    let (xv, yv, zv) = (U64::from_u64(x as u64), U64::from_u64(y as u64), U64::from_u64(z as u64));
    let mut h = xv.mul(c1).xor(yv.mul(c2)).xor(zv.mul(c3)).xor(U64::from_u64(seed).mul(c4));
    h = h.xor(h.shr(31));
    h = h.mul(c5);
    h = h.xor(h.shr(29));
    h = h.mul(c6);
    h.xor(h.shr(32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfx::hash_u;

    /// xorshift64*: a deterministic stream of inputs for the comparison.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    #[test]
    fn mulhi32_matches_wide_product() {
        let mut r = Rng(0x1234_5678_9ABC_DEF1);
        for _ in 0..200_000 {
            let (a, b) = (r.next() as u32, r.next() as u32);
            assert_eq!(mulhi32(a, b), ((a as u64 * b as u64) >> 32) as u32, "a={a:#x} b={b:#x}");
        }
        assert_eq!(mulhi32(u32::MAX, u32::MAX), 0xFFFF_FFFE);
    }

    #[test]
    fn u64_mul_matches_wrapping_mul() {
        let mut r = Rng(0xDEAD_BEEF_0BAD_F00D);
        for _ in 0..200_000 {
            let (a, b) = (r.next(), r.next());
            assert_eq!(U64::from_u64(a).mul(U64::from_u64(b)).to_u64(), a.wrapping_mul(b));
        }
    }

    #[test]
    fn hash_pair_matches_hash_u_bit_exact() {
        // Edge values first, then a few million deterministic random inputs.
        let edges = [0i64, 1, -1, 2, -2, i64::MIN, i64::MAX, 0x7FFF_FFFF, -0x8000_0000];
        let seeds = [0u64, 1, u64::MAX, 0x8000_0000_0000_0000, 42];
        for &x in &edges {
            for &y in &edges {
                for &z in &edges[..4] {
                    for &s in &seeds {
                        assert_eq!(hash_u_pair(x, y, z, s).to_u64(), hash_u(x, y, z, s), "x={x} y={y} z={z} s={s}");
                    }
                }
            }
        }
        let mut r = Rng(0x9E37_0000_0000_0001);
        for i in 0..4_000_000u32 {
            let (x, y, z, s) = (r.next() as i64, r.next() as i64, r.next() as i64, r.next());
            // Grid cells are i32 in the shader; cover that range densely as well.
            let (xi, yi) = (x as i32 as i64, y as i32 as i64);
            assert_eq!(hash_u_pair(xi, yi, z, s).to_u64(), hash_u(xi, yi, z, s), "iter {i}");
        }
    }

    #[test]
    fn from_i32_sign_extends_like_i64() {
        for v in [0i32, 1, -1, i32::MIN, i32::MAX, -12345] {
            assert_eq!(U64::from_i32(v).to_u64(), v as i64 as u64);
        }
    }

    /// WGSL `add64`: 64-bit add with carry on (hi, lo) pairs.
    fn add64(a: U64, b: U64) -> U64 {
        let lo = a.lo.wrapping_add(b.lo);
        let carry = u32::from(lo < a.lo);
        U64 { hi: a.hi.wrapping_add(b.hi).wrapping_add(carry), lo }
    }

    /// WGSL `mod12`: h mod 12 from the halves, using 2^32 mod 12 = 4.
    fn mod12(h: U64) -> u32 {
        ((h.hi % 12) * 4 + (h.lo % 12)) % 12
    }

    /// Replica of the WGSL `grad3` (same branch order and expressions).
    fn grad3_pair(h: U64, x: f32, y: f32, z: f32) -> f32 {
        match mod12(h) {
            0 => x + y,
            1 => -x + y,
            2 => x - y,
            3 => -x - y,
            4 => x + z,
            5 => -x + z,
            6 => x - z,
            7 => -x - z,
            8 => y + z,
            9 => -y + z,
            10 => y - z,
            _ => -y - z,
        }
    }

    /// Replica of the WGSL `noise3` on pairs.
    fn noise3_pair(x: f32, y: f32, z: f32, seed: U64) -> f32 {
        let (fx0, fy0, fz0) = (x.floor(), y.floor(), z.floor());
        let (xf, yf, zf) = (x - fx0, y - fy0, z - fz0);
        let (xi, yi, zi) = (fx0.clamp(-2.0e9, 2.0e9) as i32, fy0.clamp(-2.0e9, 2.0e9) as i32, fz0.clamp(-2.0e9, 2.0e9) as i32);
        let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (u, v, w) = (fade(xf), fade(yf), fade(zf));
        let hash = |dx: i32, dy: i32, dz: i32| hash_u_pair((xi + dx) as i64, (yi + dy) as i64, (zi + dz) as i64, seed.to_u64());
        let g = |dx: i32, dy: i32, dz: i32| grad3_pair(hash(dx, dy, dz), xf - dx as f32, yf - dy as f32, zf - dz as f32);
        let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = l(g(0, 0, 0), g(1, 0, 0), u);
        let x10 = l(g(0, 1, 0), g(1, 1, 0), u);
        let x01 = l(g(0, 0, 1), g(1, 0, 1), u);
        let x11 = l(g(0, 1, 1), g(1, 1, 1), u);
        l(l(x00, x10, v), l(x01, x11, v), w) * 0.9
    }

    /// Replica of the WGSL `fbm` on pairs.
    fn fbm_pair(x: f32, y: f32, z: f32, octaves: f32, seed: U64) -> f32 {
        let n = octaves.clamp(1.0, 10.0);
        let (mut sum, mut amp, mut freq, mut norm) = (0.0f32, 1.0f32, 1.0f32, 0.0f32);
        for i in 0..10u32 {
            let fi = i as f32;
            if fi >= n {
                break;
            }
            let k = (n - fi).min(1.0);
            sum += noise3_pair(x * freq, y * freq, z * freq + fi * 17.13, add64(seed, U64 { hi: 0, lo: i })) * amp * k;
            norm += amp * k;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm.max(1e-6)
    }

    #[test]
    fn kernel_noise_semantics_match_cpu_fbm() {
        let mut r = Rng(0x0BAD_5EED_1234_0001);
        let mut worst = 0f32;
        for i in 0..200_000u32 {
            let x = (r.next() % 4000) as f32 / 7.0 - 300.0;
            let y = (r.next() % 4000) as f32 / 7.0 - 300.0;
            let z = (r.next() % 1000) as f32 / 1000.0;
            let oct = 1.0 + (r.next() % 900) as f32 / 100.0;
            let seed = r.next() % 1000;
            let cpu = crate::vfx::fbm(x, y, z, oct, seed);
            let gpu = fbm_pair(x, y, z, oct, U64::from_u64(seed));
            let d = (cpu - gpu).abs();
            if d > worst {
                worst = d;
            }
            assert!(d < 1e-4, "iter {i}: x={x} y={y} z={z} oct={oct} seed={seed} cpu={cpu} gpu={gpu}");
        }
        eprintln!("worst fbm diff {worst:e}");
    }

    /// Alpha of `roughen_edges` per pixel, as `roughen_apply` in fx_roughen.wgsl computes it, with
    /// the depth taken from the CPU blur (verified separately) and the noise from the pair replica.
    #[test]
    fn kernel_apply_alpha_matches_cpu_roughen_for_every_edge_type() {
        use crate::gpufx_roughen::{roughen_input, roughen_params};
        use crate::vfx::{enc, luma};
        let (w, h) = (67usize, 41usize);
        let cx = crate::gpufx_multipass::multipass_cx();
        for kind in 0..8u32 {
            let mut e = crate::gpufx_multipass::multipass_case("roughen_edges").expect("case");
            if let Some(p) = e.params.get_mut("edge_type") {
                p.value = filmcraft_project::ParamValue::Choice(kind);
            }
            let Some(params) = roughen_params(&roughen_input(&e, &cx)).ok().flatten() else {
                panic!("kind {kind}: no effect");
            };
            let px: Vec<f32> = (0..w * h)
                .flat_map(|i| {
                    let a = if i % 5 == 0 { 0.0 } else { ((i * 29) % 97) as f32 / 97.0 };
                    [a * 0.5, a * 0.25, a * 0.75, a]
                })
                .collect();
            let alpha: Vec<f32> = px.chunks(4).map(|p| p[3]).collect();
            let depth = crate::vfx::blur_plane(&alpha, w, h, params.sigma);
            let mut cpu = crate::image::Image { w, h, px: px.clone() };
            assert!(crate::vfx::apply(&mut cpu, &e, &cx), "kind {kind}: CPU applies");
            let mut worst = 0f32;
            let mut worst_rgb = 0f32;
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let a = alpha[i];
                    if a <= 1e-6 {
                        continue;
                    }
                    let (cu, cv) =
                        ((x as f32 + params.offset_x) / (params.scale_px * params.stretch), (y as f32 + params.offset_y) * params.stretch / params.scale_px);
                    let mut n = fbm_pair(cu, cv, params.evolution, params.complexity, U64::from_u64(params.seed));
                    if params.edge_type == 3 {
                        n = 1.0 - 2.0 * n.abs();
                    }
                    let d = depth[i];
                    let mut k = ((d - 0.5 + n * params.influence * 0.5) * (1.0 + params.sharpness * 4.0) + 0.5).clamp(0.0, 1.0);
                    if params.edge_type == 2 {
                        k = if k > 0.5 { 1.0 } else { 0.0 };
                    }
                    if (4..=7).contains(&params.edge_type) {
                        let hole = fbm_pair(cu * 2.3 + 11.0, cv * 2.3, params.evolution, params.complexity, U64::from_u64(params.seed + 1));
                        if d < 0.97 && hole > 0.25 {
                            k *= 1.0 - crate::vfx::smoothstep(0.25, 0.4, hole) * (1.0 - d);
                        }
                    }
                    let diff = (cpu.px[i * 4 + 3] - a * k).abs();
                    worst = worst.max(diff);
                    // colour: the replica of the WGSL rgb branch (premultiplied output)
                    let src = [px[i * 4], px[i * 4 + 1], px[i * 4 + 2]];
                    let c = if a > 1e-6 { [src[0] / a, src[1] / a, src[2] / a] } else { [0.0; 3] };
                    let mut rgb = [src[0], src[1], src[2]];
                    if matches!(params.edge_type, 1 | 5 | 7) && d < 0.97 {
                        let t = ((1.0 - d) * 1.5).min(1.0);
                        rgb = [0, 1, 2].map(|ch| (c[ch] + (params.edge_color[ch] - c[ch]) * t) * a);
                    }
                    if params.edge_type == 6 {
                        let l = if luma(enc(c)) > 0.5 { 1.0 } else { 0.0 };
                        rgb = [l * a; 3];
                    }
                    for ch in 0..3 {
                        let rd = (cpu.px[i * 4 + ch] - rgb[ch] * k).abs();
                        if rd > worst_rgb {
                            worst_rgb = rd;
                        }
                    }
                }
            }
            eprintln!("edge_type {kind}: worst alpha diff {worst:e}, worst rgb diff {worst_rgb:e}");
            assert!(worst < 1e-4 && worst_rgb < 1e-4, "edge_type {kind}: alpha {worst}, rgb {worst_rgb}");
        }
    }
}

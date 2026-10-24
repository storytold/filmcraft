// u64 emulation for the value-noise hash (`hash_u` in crates/render/src/vfx/mod.rs).
// WGSL has no 64-bit integers: a value is a (hi, lo) pair of u32. u32 arithmetic wraps in WGSL.
// Mirror and proof: crates/render/src/vfx/hash_emul.rs (bit-exact against hash_u).

struct U64 {
    hi: u32,
    lo: u32,
}

// High 32 bits of the full 32x32 product, from 16-bit limbs.
fn mulhi32(a: u32, b: u32) -> u32 {
    let ah = a >> 16u;
    let al = a & 0xffffu;
    let bh = b >> 16u;
    let bl = b & 0xffffu;
    let t = al * bl;
    let w1 = ah * bl + (t >> 16u);
    let w2 = al * bh + (w1 & 0xffffu);
    return ah * bh + (w1 >> 16u) + (w2 >> 16u);
}

// Sign-extends an i32 cell coordinate, as `v as i64 as u64` does on the CPU.
fn u64_of_i32(v: i32) -> U64 {
    var hi = 0u;
    if (v < 0) {
        hi = 0xffffffffu;
    }
    return U64(hi, bitcast<u32>(v));
}

fn xor64(a: U64, b: U64) -> U64 {
    return U64(a.hi ^ b.hi, a.lo ^ b.lo);
}

// Logical right shift for 0 < n <= 32.
fn shr64(a: U64, n: u32) -> U64 {
    if (n >= 32u) {
        return U64(0u, a.hi >> (n - 32u));
    }
    return U64(a.hi >> n, (a.lo >> n) | (a.hi << (32u - n)));
}

// Wrapping 64-bit multiply.
fn mul64(a: U64, b: U64) -> U64 {
    let lo = a.lo * b.lo;
    let hi = mulhi32(a.lo, b.lo) + a.lo * b.hi + a.hi * b.lo;
    return U64(hi, lo);
}

// hash_u(x, y, z, seed) on pairs; the constants are the CPU's, split into halves.
fn hash_u64(x: U64, y: U64, z: U64, seed: U64) -> U64 {
    var h = xor64(xor64(mul64(x, U64(0x9E3779B9u, 0x7F4A7C15u)), mul64(y, U64(0xC2B2AE3Du, 0x27D4EB4Fu))),
                  xor64(mul64(z, U64(0x165667B1u, 0x9E3779F9u)), mul64(seed, U64(0x27D4EB2Fu, 0x165667C5u))));
    h = xor64(h, shr64(h, 31u));
    h = mul64(h, U64(0xBF58476Du, 0x1CE4E5B9u));
    h = xor64(h, shr64(h, 29u));
    h = mul64(h, U64(0x94D049BBu, 0x133111EBu));
    return xor64(h, shr64(h, 32u));
}

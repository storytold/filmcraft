// DNxHR encoding (SMPTE ST 2019-1, VC-3) in compute passes: the per-macroblock work of
// `filmcraft-dnx`'s encoder, step for step (the CPU encoder stays the reference; tests compare):
//
// - `transform`: one invocation per 8×8 block (colour blocks, then the four alpha blocks of
//   DCT-coded alpha) loads the block at the coded depth with edge replication (`prepare`'s `code`),
//   subtracts the mid level, runs the CPU's forward DCT (same basis, same order of sums) and stores
//   the DC value (`round(b0 · inv_scale)`, clamped) and the AC values pre-divided by the weights
//   (`b[z] · inv_scale · p / W`) in bitstream (zig-zag) order.
// - `rle`: one invocation per macroblock counts its lossless alpha (`rle_alpha` over its 256
//   serpentine samples, or the raw samples where those are shorter).
// - `bits`: one invocation per macroblock and quantiser counts its colour part (header, DC
//   predicted along the scan line per component, AC at `q`) and its alpha part.
// - `write`: the same walk emitting the bits of each part into its word-aligned range of the
//   output (the host places the ranges from the bits measured at each macroblock's quantiser).
//
// Divisions go through `div_rn`, correctly rounded in integer arithmetic (GPU division is not), and
// every product through `mul`, so the coefficients and amplitudes are the CPU's to the bit.
//
// Words in `outp` hold the stream's bytes in order (the first byte in the low eight bits). A write
// past a part's range, a length that is not the one measured, or a code outside its table counts
// in `status` (word 0 for `bits` and `write`, word 1 for `rle`); the host then does not trust the
// answer.

struct Params {
    // quantisers measured by one `bits` dispatch (nq of qv)
    qv: vec4<u32>,
    nq: u32,
    // macroblocks per scan line, scan lines, macroblocks
    nw: u32,
    ns: u32,
    nmb: u32,
    // blocks per macroblock (colour, then DCT alpha) and colour blocks per macroblock
    nblk: u32,
    ncol: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    cheight: u32,
    // chroma samples per macroblock across (8 or 16)
    cmbw: u32,
    // sample offsets of the Cb, Cr and alpha planes in `planes` (Y at 0)
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    // coded depth minus input depth
    shift: i32,
    // coded depth
    depth: u32,
    mid: f32,
    inv_scale: f32,
    p: f32,
    dc_max: f32,
    // always 0: keeps products from being fused into multiply-adds the CPU does not do
    zero: u32,
    total_blocks: u32,
    p_bits: u32,
    max_amp: u32,
    // the colour header's flag (4:4:4 Y'CbCr), the alpha coding (0 none, 1 DCT, 2 lossless)
    ycbcr444: u32,
    alpha_mode: u32,
    // word offset of the lossless alpha bits (one per macroblock) in `coeffs`
    rle_base: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the Y, Cb, Cr (and alpha) planes: 16-bit samples packed in pairs, the first in the low half
@group(0) @binding(1) var<storage, read> planes: array<u32>;
// basis bits [0,64), zig-zag [64,128), weights luma [128,192) chroma [192,256) (index z-1),
// block layout [256,272) (plane | x << 8 | y << 16), amplitude codes [272,528)
// (frun*128 + findex*64 + amp-1; code | length << 16), EOB 528, run codes [529,592), DC codes
// [592,608), DC code count 608
@group(0) @binding(2) var<storage, read> consts: array<u32>;
// coefficients (f32 bits, 64 per block, DC first, then AC in zig-zag order), then the lossless
// alpha bits of every macroblock (| 1 << 31 where the raw samples are coded)
@group(0) @binding(3) var<storage, read_write> coeffs: array<u32>;
// (colour, alpha) bits per macroblock and quantiser (`bits`), the coded parts (`write`)
@group(0) @binding(4) var<storage, read_write> outp: array<u32>;
// `write`: per macroblock (colour word offset, colour bits, alpha word offset, alpha bits, q)
@group(0) @binding(5) var<storage, read> aux: array<u32>;
@group(0) @binding(6) var<storage, read_write> status: array<atomic<u32>>;

const C_ZIG: u32 = 64u;
const C_WEIGHT: u32 = 128u;
const C_LAYOUT: u32 = 256u;
const C_AMP: u32 = 272u;
const C_EOB: u32 = 528u;
const C_RUN: u32 = 529u;
const C_DC: u32 = 592u;
const C_NDC: u32 = 608u;
const RAW: u32 = 0x80000000u;

fn index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.y * nwg.x * 64u + gid.x;
}

// A product rounded to f32 on its own, as the CPU rounds it.
fn mul(a: f32, b: f32) -> f32 {
    return bitcast<f32>(bitcast<u32>(a * b) ^ P.zero);
}

// f32::round: halves away from zero
fn round_away(x: f32) -> f32 {
    let t = trunc(x);
    if abs(x - t) >= 0.5 {
        return t + sign(x);
    }
    return t;
}

// `a / f32(b)` correctly rounded (to nearest, ties to even) for a whole `b` in 1..=4095: the
// quotient's mantissa by integer long division. Zero, subnormal and non-finite `a` (never a
// coefficient that matters) and results outside the normal range use the hardware division.
fn div_rn(a: f32, b: u32) -> f32 {
    let bits = bitcast<u32>(a);
    let ex = (bits >> 23u) & 0xffu;
    if ex == 0u || ex == 255u || b == 0u || b > 4095u {
        return a / f32(b);
    }
    let ma = (bits & 0x7fffffu) | 0x800000u;
    let q1 = ma / b;
    let r1 = ma % b;
    // 26 quotient bits: 24 for the mantissa, then the rounding bits
    let e = 26u - (32u - countLeadingZeros(q1));
    let q2 = (r1 << e) / b;
    let sticky = (r1 << e) % b != 0u;
    let quo = (q1 << e) | q2;
    var m = quo >> 2u;
    let low = quo & 3u;
    var biased = i32(ex) - i32(e) + 2;
    if low == 3u || (low == 2u && (sticky || (m & 1u) == 1u)) {
        m = m + 1u;
        if m == 0x1000000u {
            m = 0x800000u;
            biased = biased + 1;
        }
    }
    if biased < 1 || biased > 254 {
        return a / f32(b);
    }
    return bitcast<f32>((bits & 0x80000000u) | (u32(biased) << 23u) | (m & 0x7fffffu));
}

fn sample(i: u32) -> u32 {
    return (planes[i >> 1u] >> ((i & 1u) * 16u)) & 0xffffu;
}

// A sample of plane `pl` at the coded depth, edges repeated (`prepare`'s `code`).
fn code(pl: u32, x: u32, y: u32) -> u32 {
    var off = 0u;
    var pw = P.width;
    var ph = P.height;
    if pl == 1u || pl == 2u {
        off = select(P.off_cr, P.off_cb, pl == 1u);
        pw = P.cwidth;
        ph = P.cheight;
    } else if pl == 3u {
        off = P.off_a;
    }
    let v = sample(off + min(y, ph - 1u) * pw + min(x, pw - 1u));
    var c = 0u;
    if P.shift >= 0 {
        c = v << u32(P.shift);
    } else {
        let s = u32(-P.shift);
        c = (v + (1u << (s - 1u))) >> s;
    }
    return min(c, (1u << P.depth) - 1u);
}

// ---------------------------------------------------------------------------------------------
// Forward transform (`prepare`)
// ---------------------------------------------------------------------------------------------

var<private> blk: array<f32, 64>;

fn basis(u: u32, x: u32) -> f32 {
    return bitcast<f32>(consts[u * 8u + x]);
}

fn fdct8x8() {
    var t: array<f32, 64>;
    // horizontal: t[y][u] = Σ_x f[y][x] · B[u][x]
    for (var y = 0u; y < 8u; y = y + 1u) {
        for (var u = 0u; u < 8u; u = u + 1u) {
            var s = 0.0;
            for (var x = 0u; x < 8u; x = x + 1u) {
                s = s + mul(blk[y * 8u + x], basis(u, x));
            }
            t[y * 8u + u] = s;
        }
    }
    // vertical: F[v][u] = Σ_y t[y][u] · B[v][y]
    for (var v = 0u; v < 8u; v = v + 1u) {
        var acc: array<f32, 8>;
        for (var y = 0u; y < 8u; y = y + 1u) {
            let b = basis(v, y);
            for (var u = 0u; u < 8u; u = u + 1u) {
                acc[u] = acc[u] + mul(t[y * 8u + u], b);
            }
        }
        for (var u = 0u; u < 8u; u = u + 1u) {
            blk[v * 8u + u] = acc[u];
        }
    }
}

@compute @workgroup_size(64)
fn transform(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let g = index(gid, nwg);
    if g >= P.total_blocks {
        return;
    }
    let m = g / P.nblk;
    let k = g % P.nblk;
    let s = m / P.nw;
    let mb = m % P.nw;
    let lay = consts[C_LAYOUT + k];
    let pl = lay & 0xffu;
    var mbw = 16u;
    if pl == 1u || pl == 2u {
        mbw = P.cmbw;
    }
    let x0 = mb * mbw + ((lay >> 8u) & 0xffu);
    let y0 = s * 16u + ((lay >> 16u) & 0xffu);
    for (var j = 0u; j < 8u; j = j + 1u) {
        for (var i = 0u; i < 8u; i = i + 1u) {
            blk[j * 8u + i] = f32(code(pl, x0 + i, y0 + j)) - P.mid;
        }
    }
    fdct8x8();
    // (alpha is weighted like luma)
    var wsel = 0u;
    if pl == 1u || pl == 2u {
        wsel = 64u;
    }
    let base = g * 64u;
    let dc = clamp(round_away(mul(blk[0], P.inv_scale)), -P.dc_max - 1.0, P.dc_max);
    coeffs[base] = bitcast<u32>(dc);
    for (var r = 1u; r < 64u; r = r + 1u) {
        let z = consts[C_ZIG + r];
        let x = mul(mul(blk[z], P.inv_scale), P.p);
        coeffs[base + r] = bitcast<u32>(div_rn(x, consts[C_WEIGHT + wsel + z - 1u]));
    }
}

// ---------------------------------------------------------------------------------------------
// Bit sink: counts bits, and when `w_on` writes them MSB first into `outp[w_at..w_end)`
// ---------------------------------------------------------------------------------------------

var<private> w_on: bool;
var<private> w_bits: u32;
var<private> w_acc: u32;
var<private> w_n: u32;
var<private> w_at: u32;
var<private> w_end: u32;
var<private> w_bad: bool;

fn sink(on: bool, at: u32, end: u32) {
    w_on = on;
    w_bits = 0u;
    w_acc = 0u;
    w_n = 0u;
    w_at = at;
    w_end = end;
}

fn emit(word: u32) {
    if w_at < w_end {
        // stream byte order: the word's top byte first
        outp[w_at] = (word << 24u) | ((word & 0xff00u) << 8u) | ((word >> 8u) & 0xff00u) | (word >> 24u);
    } else {
        w_bad = true;
    }
    w_at = w_at + 1u;
}

fn put(val: u32, bits: u32) {
    if bits == 0u {
        return;
    }
    if bits > 32u {
        w_bad = true;
        return;
    }
    w_bits = w_bits + bits;
    if !w_on {
        return;
    }
    var v = val;
    if bits < 32u {
        v = val & ((1u << bits) - 1u);
    }
    let free = 32u - w_n;
    if bits < free {
        w_acc = w_acc | (v << (free - bits));
        w_n = w_n + bits;
        return;
    }
    let rem = bits - free;
    emit(w_acc | (v >> rem));
    w_n = rem;
    w_acc = 0u;
    if rem > 0u {
        w_acc = v << (32u - rem);
    }
}

// A table code: `code | length << 16`.
fn put_code(c: u32) {
    put(c & 0xffffu, c >> 16u);
}

// Pad with zero bits to a byte (in the last word).
fn finish() {
    if w_on && w_n > 0u {
        emit(w_acc);
        w_n = 0u;
    }
}

// ---------------------------------------------------------------------------------------------
// Macroblock coding (`Coder::put_colour`, `put_alpha`, `put_block`, `write_ac`)
// ---------------------------------------------------------------------------------------------

// the DC predictors of the four components along the scan line
var<private> pred: array<i32, 4>;

fn coef(i: u32) -> f32 {
    return bitcast<f32>(coeffs[i]);
}

// The predictors before macroblock `m`: its line's previous macroblock's last DC of each
// component, or 0 at the line start.
fn start_pred(m: u32) {
    pred = array<i32, 4>(0, 0, 0, 0);
    if m % P.nw == 0u {
        return;
    }
    for (var k = 0u; k < P.nblk; k = k + 1u) {
        pred[consts[C_LAYOUT + k] & 3u] = i32(coef(((m - 1u) * P.nblk + k) * 64u));
    }
}

fn dc_eta(e: i32) -> u32 {
    if e == 0 {
        return 0u;
    }
    return 32u - countLeadingZeros(u32(abs(e)));
}

// The quantised amplitude (≥ 1) of a magnitude `|t| ≥ q` (`quant`).
fn quant(t: f32, q: u32) -> u32 {
    let v = div_rn(abs(t), q) - 0.25;
    return clamp(u32(min(v, 65536.0)), 1u, P.max_amp);
}

fn put_block(m: u32, k: u32, q: u32) {
    let pl = consts[C_LAYOUT + k] & 3u;
    let base = (m * P.nblk + k) * 64u;
    let dc = i32(coef(base));
    let e = dc - pred[pl];
    pred[pl] = dc;
    let eta = dc_eta(e);
    if eta >= consts[C_NDC] {
        w_bad = true;
        return;
    }
    put_code(consts[C_DC + eta]);
    if eta > 0u {
        var rho = e;
        if e < 0 {
            rho = e + (1 << eta) - 1;
        }
        put(u32(rho), eta);
    }
    let qf = f32(q);
    var run = 0u;
    for (var r = 1u; r < 64u; r = r + 1u) {
        let x = coef(base + r);
        if abs(x) < qf {
            run = run + 1u;
            continue;
        }
        let a = quant(x, q);
        let p = (a - 1u) >> 6u;
        let frun = select(0u, 1u, run > 0u);
        let findex = select(0u, 1u, p > 0u);
        put_code(consts[C_AMP + frun * 128u + findex * 64u + ((a - 1u) & 63u)]);
        put(select(0u, 1u, x < 0.0), 1u);
        if findex == 1u {
            put(p, P.p_bits);
        }
        if frun == 1u {
            put_code(consts[C_RUN + min(run, 62u)]);
        }
        run = 0u;
    }
    put_code(consts[C_EOB]);
}

fn put_colour(m: u32, q: u32) {
    put(q, 11u);
    put(P.ycbcr444, 1u);
    for (var k = 0u; k < P.ncol; k = k + 1u) {
        put_block(m, k, q);
    }
}

fn put_dct_alpha(m: u32, q: u32) {
    put(q, 11u);
    put(0u, 1u);
    for (var k = P.ncol; k < P.nblk; k = k + 1u) {
        put_block(m, k, q);
    }
}

@compute @workgroup_size(64)
fn bits(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = index(gid, nwg);
    if idx >= P.nmb * P.nq {
        return;
    }
    let q = P.qv[idx / P.nmb];
    let m = idx % P.nmb;
    w_bad = false;
    start_pred(m);
    sink(false, 0u, 0u);
    put_colour(m, q);
    let colour = w_bits;
    var alpha = 0u;
    if P.alpha_mode == 1u {
        sink(false, 0u, 0u);
        put_dct_alpha(m, q);
        alpha = w_bits;
    } else if P.alpha_mode == 2u {
        alpha = coeffs[P.rle_base + m] & ~RAW;
    }
    if w_bad {
        atomicAdd(&status[0], 1u);
    }
    outp[idx * 2u] = colour;
    outp[idx * 2u + 1u] = alpha;
}

// ---------------------------------------------------------------------------------------------
// Lossless alpha (`rle_alpha`)
// ---------------------------------------------------------------------------------------------

var<private> px: array<u32, 256>;

// Macroblock `m`'s alpha samples in serpentine order (odd lines right to left).
fn load_px(m: u32) {
    let x0 = (m % P.nw) * 16u;
    let y0 = (m / P.nw) * 16u;
    for (var i = 0u; i < 256u; i = i + 1u) {
        let y = i / 16u;
        var x = i % 16u;
        if (y & 1u) == 1u {
            x = 15u - x;
        }
        px[i] = code(3u, x0 + x, y0 + y);
    }
}

fn predict(p: u32, d1: u32, d2: u32) -> u32 {
    if p == 1u {
        return 2u * d1 - d2;
    }
    return d1;
}

// The run-length coding of `px` (always: the caller picks the raw samples where they are shorter).
fn put_rle() {
    let d = P.depth;
    let mask = (1u << d) - 1u;
    let rice = d / 2u - 2u;
    put(1u, 4u);
    var d1 = mask >> 1u;
    var d2 = mask >> 1u;
    var count = 0u;
    loop {
        if count >= 256u {
            break;
        }
        var best_n = 0u;
        var best_p = 0u;
        var best_diff = 0u;
        for (var p = 0u; p < 2u; p = p + 1u) {
            let diff = (px[count] - predict(p, d1, d2)) & mask;
            var a1 = d1;
            var a2 = d2;
            var n = 0u;
            // (a run is at most 511 long: Elias gamma with up to 8 leading zeros)
            loop {
                if count + n >= 256u || n >= 511u {
                    break;
                }
                let v = (predict(p, a1, a2) + diff) & mask;
                if v != px[count + n] {
                    break;
                }
                a2 = a1;
                a1 = v;
                n = n + 1u;
            }
            if n > best_n {
                best_n = n;
                best_p = p;
                best_diff = diff;
            }
        }
        if best_n == 0u {
            w_bad = true;
            return;
        }
        put(best_p, 1u);
        // Elias gamma: g zeros, then n in g + 1 bits
        let g = 31u - countLeadingZeros(best_n);
        put(1u, g + 1u);
        put(best_n - (1u << g), g);
        // the difference folded to small codes for small magnitudes, Rice-coded when shorter
        var dcw = 2u * (mask - best_diff) + 1u;
        if best_diff <= (mask >> 1u) {
            dcw = 2u * best_diff;
        }
        let qr = dcw >> rice;
        if qr + 1u + rice < d {
            put(1u, 1u);
            put(((1u << qr) - 1u) << 1u, qr + 1u);
            put(dcw & ((1u << rice) - 1u), rice);
        } else {
            put(0u, 1u);
            put(dcw, d);
        }
        if best_n >= 2u {
            d1 = px[count + best_n - 1u];
            d2 = px[count + best_n - 2u];
        } else {
            d2 = d1;
            d1 = px[count];
        }
        count = count + best_n;
    }
}

fn put_raw() {
    put(0u, 4u);
    for (var i = 0u; i < 256u; i = i + 1u) {
        put(px[i], P.depth);
    }
}

@compute @workgroup_size(64)
fn rle(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let m = index(gid, nwg);
    if m >= P.nmb {
        return;
    }
    w_bad = false;
    load_px(m);
    sink(false, 0u, 0u);
    put_rle();
    var n = w_bits;
    let raw = 4u + 256u * P.depth;
    if n > raw {
        n = raw | RAW;
    }
    if w_bad {
        atomicAdd(&status[1], 1u);
    }
    coeffs[P.rle_base + m] = n;
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

// Start a part of `want` bits at word `at`.
fn part(at: u32, want: u32) {
    sink(true, at, at + (want + 31u) / 32u);
}

// Close a part: its length must be the one measured.
fn close(want: u32) {
    finish();
    if w_bits != want {
        w_bad = true;
    }
}

@compute @workgroup_size(64)
fn write(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let m = index(gid, nwg);
    if m >= P.nmb {
        return;
    }
    let a = m * 5u;
    let q = aux[a + 4u];
    w_bad = false;
    start_pred(m);
    part(aux[a], aux[a + 1u]);
    put_colour(m, q);
    close(aux[a + 1u]);
    if P.alpha_mode == 1u {
        part(aux[a + 2u], aux[a + 3u]);
        put_dct_alpha(m, q);
        close(aux[a + 3u]);
    } else if P.alpha_mode == 2u {
        part(aux[a + 2u], aux[a + 3u]);
        load_px(m);
        if (coeffs[P.rle_base + m] & RAW) != 0u {
            put_raw();
        } else {
            put_rle();
        }
        close(aux[a + 3u]);
    }
    if w_bad {
        atomicAdd(&status[0], 1u);
    }
}

// ProRes encoding (SMPTE RDD 36) in compute passes: the per-block work of `filmcraft-prores`'s
// encoder, step for step (the CPU encoder stays the reference; tests compare):
//
// - `fdct`: one invocation per 8×8 block loads the block with edge replication into the 12-bit
//   signed domain, runs the CPU's separable forward DCT (same basis, same order of sums) and
//   writes the coefficients in the CPU's per-slice order: a slice's luma blocks (4 per macroblock),
//   then its Cb blocks, then its Cr blocks (2 per macroblock, or 4 in 4:4:4: TL, BL, TR, BR).
// - `bits`: one invocation per slice component and quantiser quantises the component and runs the
//   counting form of `encode_component`, writing its bits padded to whole bytes.
// - `write`: the same walk emitting the bits into the component's word-aligned range of the output
//   (the host places the ranges from the bits measured at each slice's quantiser).
// - `alpha`: one invocation per slice runs `encode_alpha_slice` (16-bit alpha, run-length coded)
//   into the slice's worst-case region; `alpha_pack` packs the regions' coded bytes together.
//
// Words in `outp` hold the stream's bytes in order (the first byte in the low eight bits), so the
// host reads the coded bytes as they are. A write past a component's range, a length that is not
// the one measured, or a value outside the code's range counts in `status` (word 0 for `bits` and
// `write`, word 1 for `alpha`); the host then does not trust the answer.

struct Params {
    n_slices: u32,
    mb_w: u32,
    mb_h: u32,
    // chroma blocks per macroblock and plane (2 or 4), blocks per macroblock (8 or 12)
    cpm: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    bpm: u32,
    // input sample → 12-bit domain
    scale: f32,
    // sample offsets of the Cb, Cr and alpha planes in `planes` (Y at 0)
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    // quantisers measured by one `bits` dispatch (nq of qv)
    nq: u32,
    // significant bits of the input samples
    depth: u32,
    // always 0: keeps products from being fused into multiply-adds the CPU does not do
    zero: u32,
    // word offset of the alpha regions in the alpha buffer (after one length per slice)
    alpha_base: u32,
    qv: vec4<u32>,
    total_blocks: u32,
    // word offset of the macroblock-column table in `slices`
    mbcol_base: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the Y, Cb, Cr (and alpha) planes: 16-bit samples packed in pairs, the first in the low half
@group(0) @binding(1) var<storage, read> planes: array<u32>;
// 4 words per slice (mb_x, mb_y, mbs, first block), then per macroblock column of a row:
// first column of its slice | slice width << 16
@group(0) @binding(2) var<storage, read> slices: array<u32>;
// basis bits [0,64), scan [64,128), codebooks [128,162) (first DC, DC×7, run×16, level×10;
// rice | exp << 8 | rice_q_max << 16), AC rounding bits 162, DC / run / level contexts 163..166,
// 1 / (4 qscale(q)) bits for q = 0..224 from 192
@group(0) @binding(3) var<storage, read> consts: array<u32>;
// coefficients (f32 bits, 64 per block), or the alpha regions for `alpha_pack`
@group(0) @binding(4) var<storage, read_write> work: array<u32>;
// bits per component (`bits`), the coded stream (`write`), lengths + alpha regions (`alpha`),
// packed alpha (`alpha_pack`)
@group(0) @binding(5) var<storage, read_write> outp: array<u32>;
// `write`: (word offset, bits) per component then the quantiser per slice; `alpha_pack`: word
// offset per slice
@group(0) @binding(6) var<storage, read> aux: array<u32>;
@group(0) @binding(7) var<storage, read_write> status: array<atomic<u32>>;

const C_SCAN: u32 = 64u;
const CB_FIRST_DC: u32 = 128u;
const CB_DC: u32 = 129u;
const CB_RUN: u32 = 136u;
const CB_LEVEL: u32 = 152u;
const C_AC_ROUND: u32 = 162u;
const C_DC_CTX: u32 = 163u;
const C_RUN_CTX: u32 = 164u;
const C_LEVEL_CTX: u32 = 165u;
const C_INV: u32 = 192u;
const ALPHA_WORDS_PER_MB: u32 = 144u;

fn index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.y * nwg.x * 64u + gid.x;
}

// A product rounded to f32 on its own, as the CPU rounds it.
fn mul(a: f32, b: f32) -> f32 {
    return bitcast<f32>(bitcast<u32>(a * b) ^ P.zero);
}

fn sample(off: u32, i: u32) -> u32 {
    let k = off + i;
    return (planes[k >> 1u] >> ((k & 1u) * 16u)) & 0xffffu;
}

// ---------------------------------------------------------------------------------------------
// Forward DCT (`filmcraft-prores` `load_block` + `fdct8x8`)
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
fn fdct(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let g = index(gid, nwg);
    if g >= P.total_blocks {
        return;
    }
    let m = g / P.bpm;
    let k = g % P.bpm;
    let mb_y = m / P.mb_w;
    let mb_x = m % P.mb_w;
    let col = slices[P.mbcol_base + mb_x];
    let start = col & 0xffffu;
    let smbs = col >> 16u;
    let l = mb_x - start;
    let first = (mb_y * P.mb_w + start) * P.bpm;
    var dest = 0u;
    var off = 0u;
    var stride = P.width;
    var bx = 0u;
    var by = 0u;
    if k < 4u {
        dest = first + l * 4u + k;
        bx = mb_x * 16u + (k & 1u) * 8u;
        by = mb_y * 16u + (k >> 1u) * 8u;
    } else {
        var j = k - 4u;
        var c = 0u;
        off = P.off_cb;
        if j >= P.cpm {
            j = j - P.cpm;
            c = 1u;
            off = P.off_cr;
        }
        dest = first + 4u * smbs + c * P.cpm * smbs + l * P.cpm + j;
        stride = P.cwidth;
        if P.cpm == 4u {
            bx = mb_x * 16u + (j >> 1u) * 8u;
            by = mb_y * 16u + (j & 1u) * 8u;
        } else {
            bx = mb_x * 8u;
            by = mb_y * 16u + j * 8u;
        }
    }
    for (var yy = 0u; yy < 8u; yy = yy + 1u) {
        let y = min(by + yy, P.height - 1u);
        for (var xx = 0u; xx < 8u; xx = xx + 1u) {
            let x = min(bx + xx, stride - 1u);
            blk[yy * 8u + xx] = f32(sample(off, y * stride + x)) * P.scale - 2048.0;
        }
    }
    fdct8x8();
    for (var i = 0u; i < 64u; i = i + 1u) {
        work[dest * 64u + i] = bitcast<u32>(blk[i]);
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
    w_bad = false;
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

// Pad with zero bits to a byte (in the last word).
fn finish() {
    if w_on && w_n > 0u {
        emit(w_acc);
        w_n = 0u;
    }
}

// A Rice / Exp-Golomb codeword (`filmcraft-prores` `Writer::put_cw` / `cw_len`).
fn put_cw(cb: u32, val: u32) {
    let rice = cb & 0xffu;
    let ex = (cb >> 8u) & 0xffu;
    let rq = (cb >> 16u) & 0xffu;
    let span = (rq + 1u) << rice;
    if val < span {
        put(0u, val >> rice);
        put(1u, 1u);
        put(val & ((1u << rice) - 1u), rice);
        return;
    }
    let v = (val - span) + (1u << ex);
    if v < (1u << ex) {
        // (beyond 32 bits: no ProRes level is this large)
        w_bad = true;
        return;
    }
    let nbits = 32u - countLeadingZeros(v);
    let zeros = rq + 1u + (nbits - ex - 1u);
    put(0u, min(zeros, 32u));
    if zeros > 32u {
        put(0u, zeros - 32u);
    }
    put(v, nbits);
}

// ---------------------------------------------------------------------------------------------
// Quantisation and entropy coding (`quantise` + `encode_component`)
// ---------------------------------------------------------------------------------------------

fn coef(b: u32, i: u32) -> f32 {
    return bitcast<f32>(work[b * 64u + i]);
}

// f32::round: halves away from zero
fn q_dc(c: f32, inv: f32) -> i32 {
    let x = mul(c, inv);
    let t = trunc(x);
    var r = t;
    if abs(x - t) >= 0.5 {
        r = t + sign(x);
    }
    return i32(r);
}

fn q_ac(c: f32, inv: f32) -> i32 {
    let x = mul(c, inv);
    let a = i32(abs(x) + bitcast<f32>(consts[C_AC_ROUND]));
    if x < 0.0 {
        return -a;
    }
    return a;
}

fn encode_component(base: u32, n: u32, inv: f32) {
    // DC
    let first = q_dc(coef(base, 0u), inv);
    var zz = u32(first) << 1u;
    if first < 0 {
        zz = u32(-first) * 2u - 1u;
    }
    put_cw(consts[CB_FIRST_DC], zz);
    var prev = first;
    var ctx = consts[C_DC_CTX];
    var sgn = false;
    for (var b = 1u; b < n; b = b + 1u) {
        let dc = q_dc(coef(base + b, 0u), inv);
        let delta = dc - prev;
        prev = dc;
        var code = 0u;
        if delta == 0 {
            sgn = false;
        } else {
            let neg = delta < 0;
            let mag = u32(abs(delta));
            code = 2u * mag;
            if neg != sgn {
                code = 2u * mag - 1u;
            }
            sgn = neg;
        }
        put_cw(consts[CB_DC + min(ctx, 6u)], code);
        ctx = code;
    }
    // AC, interleaved across blocks
    var run = 0u;
    var run_ctx = consts[C_RUN_CTX];
    var lev_ctx = consts[C_LEVEL_CTX];
    for (var si = 1u; si < 64u; si = si + 1u) {
        let nat = consts[C_SCAN + si];
        for (var b = 0u; b < n; b = b + 1u) {
            let l = q_ac(coef(base + b, nat), inv);
            if l == 0 {
                run = run + 1u;
            } else {
                put_cw(consts[CB_RUN + min(run_ctx, 15u)], run);
                run_ctx = run;
                let a = u32(abs(l));
                put_cw(consts[CB_LEVEL + min(lev_ctx, 9u)], a - 1u);
                lev_ctx = a;
                put(select(0u, 1u, l < 0), 1u);
                run = 0u;
            }
        }
    }
}

// First block and block count of component `c` of slice `s`.
fn comp_range(s: u32, c: u32) -> vec2<u32> {
    let mbs = slices[s * 4u + 2u];
    let first = slices[s * 4u + 3u];
    if c == 0u {
        return vec2<u32>(first, 4u * mbs);
    }
    return vec2<u32>(first + (4u + (c - 1u) * P.cpm) * mbs, P.cpm * mbs);
}

fn inv_q(q: u32) -> f32 {
    return bitcast<f32>(consts[C_INV + min(q, 224u)]);
}

@compute @workgroup_size(64)
fn bits(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = index(gid, nwg);
    let per = P.n_slices * 3u;
    if idx >= per * P.nq {
        return;
    }
    let v = idx / per;
    let r = idx % per;
    let range = comp_range(r / 3u, r % 3u);
    sink(false, 0u, 0u);
    encode_component(range.x, range.y, inv_q(P.qv[v]));
    if w_bad {
        atomicAdd(&status[0], 1u);
    }
    outp[idx] = (w_bits + 7u) / 8u * 8u;
}

@compute @workgroup_size(64)
fn write(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = index(gid, nwg);
    if idx >= P.n_slices * 3u {
        return;
    }
    let s = idx / 3u;
    let at = aux[idx * 2u];
    let want = aux[idx * 2u + 1u];
    let range = comp_range(s, idx % 3u);
    sink(true, at, at + (want + 31u) / 32u);
    encode_component(range.x, range.y, inv_q(aux[P.n_slices * 6u + s]));
    finish();
    if w_bad || (w_bits + 7u) / 8u * 8u != want {
        atomicAdd(&status[0], 1u);
    }
}

// ---------------------------------------------------------------------------------------------
// Alpha (`encode_alpha_slice` + `encode_alpha`, 16-bit coding)
// ---------------------------------------------------------------------------------------------

// Alpha value `i` of a slice (raster order over its 16 lines, edge-replicated), at 16 bits.
fn alpha_val(x0: u32, y0: u32, sw: u32, i: u32) -> u32 {
    let y = min(y0 + i / sw, P.height - 1u);
    let x = min(x0 + i % sw, P.width - 1u);
    let d = P.depth;
    let v = sample(P.off_a, y * P.width + x) & ((1u << d) - 1u);
    if d >= 16u {
        return v;
    }
    let down = min(u32(max(2 * i32(d) - 16, 0)), d);
    return ((v << (16u - d)) | (v >> down)) & 0xffffu;
}

@compute @workgroup_size(64)
fn alpha(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let s = index(gid, nwg);
    if s >= P.n_slices {
        return;
    }
    let mb_x = slices[s * 4u];
    let mb_y = slices[s * 4u + 1u];
    let mbs = slices[s * 4u + 2u];
    let x0 = mb_x * 16u;
    let y0 = mb_y * 16u;
    let sw = mbs * 16u;
    let count = sw * 16u;
    let region = P.alpha_base + (mb_y * P.mb_w + mb_x) * ALPHA_WORDS_PER_MB;
    sink(true, region, region + mbs * ALPHA_WORDS_PER_MB);
    var prev = 0xffffu;
    var i = 0u;
    loop {
        if i >= count {
            break;
        }
        let v = alpha_val(x0, y0, sw, i);
        let diff = (v - prev) & 0xffffu;
        var sd = i32(diff);
        if diff > 0x7fffu {
            sd = i32(diff) - 65536;
        }
        if sd != 0 && abs(sd) <= 64 {
            put(0u, 1u);
            put((2u * u32(abs(sd)) - 2u) | select(0u, 1u, sd < 0), 7u);
        } else {
            put(1u, 1u);
            put(diff, 16u);
        }
        prev = v;
        i = i + 1u;
        // a run after every value, including the last (RDD 36 §5.3.3)
        var run = 0u;
        loop {
            if i + run >= count || run >= 2047u || alpha_val(x0, y0, sw, i + run) != prev {
                break;
            }
            run = run + 1u;
        }
        if run == 0u {
            put(1u, 1u);
            continue;
        }
        put(0u, 1u);
        if run < 16u {
            put(run, 4u);
        } else {
            put(0u, 4u);
            put(run, 11u);
        }
        i = i + run;
    }
    finish();
    outp[s] = (w_bits + 7u) / 8u;
    if w_bad {
        atomicAdd(&status[1], 1u);
    }
}

@compute @workgroup_size(64)
fn alpha_pack(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let s = index(gid, nwg);
    if s >= P.n_slices {
        return;
    }
    let mb_x = slices[s * 4u];
    let mb_y = slices[s * 4u + 1u];
    let mbs = slices[s * 4u + 2u];
    let region = P.alpha_base + (mb_y * P.mb_w + mb_x) * ALPHA_WORDS_PER_MB;
    let words = min((work[s] + 3u) / 4u, mbs * ALPHA_WORDS_PER_MB);
    let dst = aux[s];
    for (var k = 0u; k < words; k = k + 1u) {
        outp[dst + k] = work[region + k];
    }
}

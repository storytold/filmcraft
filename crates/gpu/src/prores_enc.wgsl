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
// An interlaced frame is two field pictures, each `mb_h` macroblock rows of every second frame
// line (`first[p]`, `first[p] + step`, …, `lines[p]` of them). Macroblock rows are counted over the
// pictures one after the other (the second picture's row 0 is row `mb_h`), so the coefficients,
// the slice records and the alpha regions of the second picture follow the first's. Every
// component is coded with the scan at `scan` (progressive or interlaced).
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
    // frame lines between a picture's lines (1, or 2 for fields), each picture's first frame line
    // and number of lines (`first1`, `lines1`: the second field)
    step: u32,
    first0: u32,
    first1: u32,
    lines0: u32,
    lines1: u32,
    // word offset of the scan order in `consts`
    scan: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the Y, Cb, Cr (and alpha) planes: 16-bit samples packed in pairs, the first in the low half
@group(0) @binding(1) var<storage, read> planes: array<u32>;
// 4 words per slice (mb_x, macroblock row over the pictures, mbs, first block), then per
// macroblock column of a row:
// first column of its slice | slice width << 16
@group(0) @binding(2) var<storage, read> slices: array<u32>;
// basis bits [0,64), progressive scan [64,128), codebooks [128,162) (first DC, DC×7, run×16,
// level×10; rice | exp << 8 | rice_q_max << 16), AC rounding bits 162, DC / run / level contexts
// 163..166, 1 / (4 qscale(q)) bits for q = 0..224 from 192, interlaced scan [417,481)
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

// The frame line of line `y` of picture `pic`, edge-replicated below the picture's last line.
fn frame_line(pic: u32, y: u32) -> u32 {
    if pic == 0u {
        return P.first0 + min(y, P.lines0 - 1u) * P.step;
    }
    return P.first1 + min(y, P.lines1 - 1u) * P.step;
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
    // the macroblock row over the pictures, the picture, and the row within it
    let row = m / P.mb_w;
    let pic = row / P.mb_h;
    let mb_y = row % P.mb_h;
    let mb_x = m % P.mb_w;
    let col = slices[P.mbcol_base + mb_x];
    let start = col & 0xffffu;
    let smbs = col >> 16u;
    let l = mb_x - start;
    let first = (row * P.mb_w + start) * P.bpm;
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
        let y = frame_line(pic, by + yy);
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
        let nat = consts[P.scan + si];
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

// Alpha value `i` of a slice of picture `pic` (raster order over its 16 lines of the picture,
// edge-replicated), at 16 bits.
fn alpha_val(pic: u32, x0: u32, y0: u32, sw: u32, i: u32) -> u32 {
    let y = frame_line(pic, y0 + i / sw);
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
    let pic = mb_y / P.mb_h;
    let x0 = mb_x * 16u;
    let y0 = (mb_y % P.mb_h) * 16u;
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
        let v = alpha_val(pic, x0, y0, sw, i);
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
            if i + run >= count || run >= 2047u || alpha_val(pic, x0, y0, sw, i + run) != prev {
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

// ---------------------------------------------------------------------------------------------
// Rate control (`choose_quantisers`), with no read-back until the answer
// ---------------------------------------------------------------------------------------------
//
// `rc_bits` measures every slice component at the quantiser the state asks for into the next slot
// of the slot table; `rc_pick` (one workgroup) adds the slot up and runs `smallest_fitting`'s
// search as a state machine until it needs a quantiser not measured yet (or has the answer, and
// then the one finer quantiser the refinement reads). The host alternates the two as often as the
// CPU search can measure (`RC_ROUNDS`); once the state is done the later rounds return at once.
// `rc_refine` (one workgroup) is the greedy one-step-finer refinement in slice order, `rc_final`
// (one invocation per slice) the 16-bit size guard, each slice raised until it fits, and copies
// the rows the host keeps (the uniform quantiser's and the finer one's).
//
// `outp` holds the state words (R_*), the slots' quantisers and 64-bit totals, the answer (one
// quantiser per slice), the kept rows (3 words per slice each), every slice's current bits, then
// the slot table (3 words per slice per slot). `aux` holds the budget (low, high word), the hint,
// and every slice's limit in bits. Anything the CPU algorithm would not survive (a slot table
// overflow, a slice's bits beyond 32 bits, a state that never finishes) counts in `status[0]`.

const R_PHASE: u32 = 0u;
// the quantiser `rc_bits` measures next
const R_Q: u32 = 1u;
const R_LO: u32 = 2u;
const R_HI: u32 = 3u;
const R_STEP: u32 = 4u;
const R_QG: u32 = 5u;
const R_DONE: u32 = 6u;
const R_SLOTS: u32 = 7u;
// the kept rows: quantiser (0: none) and slot
const R_ROW0_Q: u32 = 8u;
const R_ROW0_K: u32 = 9u;
const R_ROW1_Q: u32 = 10u;
const R_ROW1_K: u32 = 11u;
const R_SLOT_Q: u32 = 16u;
const R_SLOT_TOT: u32 = 32u;
const R_QS: u32 = 64u;
const MAX_SLOTS: u32 = 16u;

const PH_FIRST: u32 = 0u;
const PH_DOWN: u32 = 1u;
const PH_UP: u32 = 2u;
const PH_BISECT: u32 = 3u;
const PH_FINER: u32 = 4u;

const Q_MAX: u32 = 224u;

fn rows_base() -> u32 {
    return R_QS + P.n_slices;
}

fn cur_base() -> u32 {
    return rows_base() + P.n_slices * 6u;
}

fn table_base() -> u32 {
    return cur_base() + P.n_slices;
}

fn fail() {
    atomicAdd(&status[0], 1u);
}

// 64-bit unsigned values as (low, high) words
fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    return vec2<u32>(lo, a.y + b.y + select(0u, 1u, lo < a.x));
}

fn sub64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x - b.x, a.y - b.y - select(0u, 1u, a.x < b.x));
}

fn le64(a: vec2<u32>, b: vec2<u32>) -> bool {
    return a.y < b.y || (a.y == b.y && a.x <= b.x);
}

fn budget() -> vec2<u32> {
    return vec2<u32>(aux[0], aux[1]);
}

fn slot_total(k: u32) -> vec2<u32> {
    return vec2<u32>(outp[R_SLOT_TOT + k * 2u], outp[R_SLOT_TOT + k * 2u + 1u]);
}

// The slot holding quantiser `q`, MAX_SLOTS when it was not measured.
fn slot_of(q: u32) -> u32 {
    let n = min(outp[R_SLOTS], MAX_SLOTS);
    for (var k = 0u; k < n; k = k + 1u) {
        if outp[R_SLOT_Q + k] == q {
            return k;
        }
    }
    return MAX_SLOTS;
}

// Slice `s`'s bits in slot `k` (its components added); `rc_bad` when they pass 32 bits.
var<private> rc_bad: bool;

fn slot_slice(k: u32, s: u32) -> u32 {
    let at = table_base() + (k * P.n_slices + s) * 3u;
    return add_bits(add_bits(outp[at], outp[at + 1u]), outp[at + 2u]);
}

fn add_bits(a: u32, b: u32) -> u32 {
    let c = a + b;
    if c < a {
        rc_bad = true;
    }
    return c;
}

@compute @workgroup_size(64)
fn rc_bits(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = index(gid, nwg);
    let per = P.n_slices * 3u;
    let k = outp[R_SLOTS];
    if idx >= per || outp[R_DONE] != 0u || k >= MAX_SLOTS {
        return;
    }
    let range = comp_range(idx / 3u, idx % 3u);
    sink(false, 0u, 0u);
    encode_component(range.x, range.y, inv_q(outp[R_Q]));
    if w_bad {
        fail();
    }
    outp[table_base() + k * per + idx] = (w_bits + 7u) / 8u * 8u;
}

// The search's answer `qg`: then the finer quantiser, if the refinement reads it and it was not
// measured yet (a guard: when the refinement reads it, it is the bracket's `lo`, measured).
fn rc_found(qg: u32) {
    outp[R_QG] = qg;
    let k = slot_of(qg);
    if k == MAX_SLOTS {
        fail();
        outp[R_DONE] = 1u;
        return;
    }
    if qg > 1u && !le64(budget(), slot_total(k)) && slot_of(qg - 1u) == MAX_SLOTS {
        outp[R_PHASE] = PH_FINER;
        outp[R_Q] = qg - 1u;
        return;
    }
    outp[R_DONE] = 1u;
}

// `smallest_fitting`'s steps: towards finer quantisers, towards coarser ones, bisection.
fn rc_down() {
    let hi = outp[R_HI];
    if hi == 1u {
        rc_found(1u);
        return;
    }
    let by = outp[R_STEP];
    outp[R_Q] = select(1u, hi - by, hi > by);
}

fn rc_up() {
    let lo = outp[R_LO];
    if lo == Q_MAX {
        rc_found(Q_MAX);
        return;
    }
    outp[R_Q] = min(lo + outp[R_STEP], Q_MAX);
}

fn rc_bisect() {
    let lo = outp[R_LO];
    let hi = outp[R_HI];
    if hi - lo > 1u {
        outp[R_PHASE] = PH_BISECT;
        outp[R_Q] = lo + (hi - lo) / 2u;
        return;
    }
    rc_found(hi);
}

// The answer to the question `q`: whether its frame fits.
fn rc_step(q: u32, fit: bool) {
    let phase = outp[R_PHASE];
    let wider = min(outp[R_STEP] * 2u, 255u);
    if phase == PH_FIRST {
        outp[R_STEP] = 1u;
        if fit {
            outp[R_HI] = q;
            outp[R_PHASE] = PH_DOWN;
            rc_down();
        } else {
            outp[R_LO] = q;
            outp[R_PHASE] = PH_UP;
            rc_up();
        }
    } else if phase == PH_DOWN {
        if fit {
            outp[R_HI] = q;
            outp[R_STEP] = wider;
            rc_down();
        } else {
            outp[R_LO] = q;
            rc_bisect();
        }
    } else if phase == PH_UP {
        if fit {
            outp[R_HI] = q;
            rc_bisect();
        } else {
            outp[R_LO] = q;
            outp[R_STEP] = wider;
            rc_up();
        }
    } else if phase == PH_BISECT {
        if fit {
            outp[R_HI] = q;
        } else {
            outp[R_LO] = q;
        }
        rc_bisect();
    } else {
        outp[R_DONE] = 1u;
    }
}

// Run the state machine while its question is answered by a slot.
fn rc_advance() {
    for (var i = 0u; i < 64u; i = i + 1u) {
        if outp[R_DONE] != 0u {
            return;
        }
        let q = outp[R_Q];
        let k = slot_of(q);
        if k == MAX_SLOTS {
            return;
        }
        rc_step(q, le64(slot_total(k), budget()));
    }
    fail();
    outp[R_DONE] = 1u;
}

var<workgroup> wg_word: u32;
var<workgroup> wg_sum: array<vec2<u32>, 64>;

@compute @workgroup_size(64)
fn rc_pick(@builtin(local_invocation_index) lid: u32) {
    if lid == 0u {
        wg_word = select(0u, 1u, outp[R_DONE] != 0u);
    }
    if workgroupUniformLoad(&wg_word) != 0u {
        return;
    }
    let k = outp[R_SLOTS];
    let per = P.n_slices * 3u;
    var acc = vec2<u32>(0u, 0u);
    if k < MAX_SLOTS {
        let at = table_base() + k * per;
        for (var i = lid; i < per; i = i + 64u) {
            acc = add64(acc, vec2<u32>(outp[at + i], 0u));
        }
    }
    wg_sum[lid] = acc;
    for (var w = 32u; w > 0u; w = w / 2u) {
        workgroupBarrier();
        if lid < w {
            wg_sum[lid] = add64(wg_sum[lid], wg_sum[lid + w]);
        }
    }
    if lid != 0u {
        return;
    }
    if k >= MAX_SLOTS {
        fail();
        outp[R_DONE] = 1u;
        return;
    }
    outp[R_SLOT_Q + k] = outp[R_Q];
    outp[R_SLOT_TOT + k * 2u] = wg_sum[0].x;
    outp[R_SLOT_TOT + k * 2u + 1u] = wg_sum[0].y;
    outp[R_SLOTS] = k + 1u;
    rc_advance();
}

// `choose_quantisers` after the search: every slice at the uniform quantiser, then one step finer
// on as many slices as the budget left allows, in slice order. One workgroup: the slices' bits are
// read in parallel, a chunk at a time, and one invocation takes the decisions in order.
const RF_CHUNK: u32 = 512u;
var<workgroup> rf_cur: array<u32, 512>;
var<workgroup> rf_finer: array<u32, 512>;
var<workgroup> rf_take: array<u32, 512>;

// The kept rows; whether the refinement runs (1), not (0), or the search failed (2).
fn refine_mode() -> u32 {
    let qg = outp[R_QG];
    let kg = slot_of(qg);
    if outp[R_DONE] == 0u || kg == MAX_SLOTS || qg < 1u || qg > Q_MAX {
        fail();
        outp[R_DONE] = 0u;
        return 2u;
    }
    let kf = slot_of(qg - 1u);
    outp[R_ROW0_Q] = qg;
    outp[R_ROW0_K] = kg;
    outp[R_ROW1_Q] = select(0u, qg - 1u, kf != MAX_SLOTS);
    outp[R_ROW1_K] = kf;
    if qg == 1u || le64(budget(), slot_total(kg)) {
        return 0u;
    }
    if kf == MAX_SLOTS {
        fail();
        outp[R_DONE] = 0u;
        return 2u;
    }
    return 1u;
}

@compute @workgroup_size(64)
fn rc_refine(@builtin(local_invocation_index) lid: u32) {
    if lid == 0u {
        wg_word = refine_mode();
    }
    let mode = workgroupUniformLoad(&wg_word);
    if mode == 2u {
        return;
    }
    let n = P.n_slices;
    let qg = outp[R_QG];
    let kg = slot_of(qg);
    let kf = slot_of(qg - 1u);
    let cur = cur_base();
    // (thread 0's: the bits the refinement may still spend)
    var left = sub64(budget(), slot_total(kg));
    rc_bad = false;
    for (var base = 0u; base < n; base = base + RF_CHUNK) {
        for (var j = lid; j < RF_CHUNK && base + j < n; j = j + 64u) {
            let s = base + j;
            rf_cur[j] = slot_slice(kg, s);
            rf_take[j] = 0u;
            if mode == 1u {
                let f = slot_slice(kf, s);
                rf_finer[j] = f;
                rf_take[j] = select(0u, 1u, f <= aux[4u + s]);
            }
        }
        workgroupBarrier();
        if lid == 0u && mode == 1u {
            for (var j = 0u; j < RF_CHUNK && base + j < n; j = j + 1u) {
                let f = rf_finer[j];
                let c = rf_cur[j];
                let d = vec2<u32>(select(0u, f - c, f > c), 0u);
                if rf_take[j] == 1u && le64(d, left) {
                    left = sub64(left, d);
                } else {
                    rf_take[j] = 0u;
                }
            }
        }
        workgroupBarrier();
        for (var j = lid; j < RF_CHUNK && base + j < n; j = j + 64u) {
            let take = rf_take[j] == 1u;
            outp[R_QS + base + j] = select(qg, qg - 1u, take);
            outp[cur + base + j] = select(rf_cur[j], rf_finer[j], take);
        }
        workgroupBarrier();
    }
    if rc_bad {
        fail();
    }
}

// The kept rows, then the size guard: a slice over its limit is raised one quantiser at a time
// until it fits (or reaches Q_MAX), as the CPU's rounds raise it.
@compute @workgroup_size(64)
fn rc_final(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let s = index(gid, nwg);
    let n = P.n_slices;
    if s >= n || outp[R_DONE] == 0u {
        return;
    }
    let table = table_base();
    let rows = rows_base();
    for (var c = 0u; c < 3u; c = c + 1u) {
        outp[rows + s * 3u + c] = outp[table + (outp[R_ROW0_K] * n + s) * 3u + c];
        if outp[R_ROW1_Q] != 0u {
            outp[rows + (n + s) * 3u + c] = outp[table + (outp[R_ROW1_K] * n + s) * 3u + c];
        }
    }
    let limit = aux[4u + s];
    var q = outp[R_QS + s];
    var have = outp[cur_base() + s];
    rc_bad = false;
    loop {
        if have <= limit || q >= Q_MAX {
            break;
        }
        q = q + 1u;
        have = 0u;
        for (var c = 0u; c < 3u; c = c + 1u) {
            let range = comp_range(s, c);
            sink(false, 0u, 0u);
            encode_component(range.x, range.y, inv_q(q));
            if w_bad {
                rc_bad = true;
            }
            have = add_bits(have, (w_bits + 7u) / 8u * 8u);
        }
    }
    if rc_bad {
        fail();
    }
    outp[R_QS + s] = q;
}

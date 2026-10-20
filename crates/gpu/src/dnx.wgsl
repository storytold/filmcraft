// DNxHD / DNxHR (VC-3, SMPTE ST 2019-1) scan line decoding in compute passes, mirroring
// `filmcraft-dnx`'s CPU decoder step for step (which stays the reference; tests compare).
//
// A macroblock scan line is one entropy-coded run: the DC predictors run through it and its
// macroblocks have no offsets of their own, so decoding it in one invocation is one long serial
// chain (tens of milliseconds for a 4K frame). Instead each line is cut into segments of
// `seg_bits` bits, decoded in parallel and stitched together:
//
// - `sync`: one invocation per segment parses VLC elements from a little before the segment's
//   start (assuming an AC element boundary, and a DC value after every end of block) and takes the
//   end of the first block it finds inside the segment as the segment's candidate block boundary.
//   VLC parses resynchronise quickly, so most candidates are true boundaries; none has to be.
// - `trans`: one invocation per segment and hypothesis of the block index at its candidate parses
//   whole blocks from there until it ends a block exactly on a later segment's candidate (or
//   gives up after `cap` candidates): how many blocks that took, the sums of the DC differences
//   per component and the last quantiser scale read.
// - `scan`: one invocation per line follows the true chain from the line's start through these
//   records: every landing of a parse from a true state is a true block boundary, so each segment
//   reached gets the exact decoder state at its candidate (block index, DC predictors, quantiser).
// - `decode`: one invocation per reached segment decodes its blocks from that state and writes
//   them dequantised into the coefficient buffer (one slot of 64 floats per block).
// - `idct`: one invocation per 8×8 block runs the inverse DCT (the CPU's separable f32 IDCT, in
//   the same order; the same code as `prores.wgsl`'s), maps the samples to codes and writes them
//   into the output planes.
//
// Only Y'CbCr 4:2:2 streams without alpha come here (see `FrameLayout::plain_ycbcr`). Output
// planes are rows of packed pairs as in `prores.wgsl`: half floats `code / 2^depth`, or the codes
// themselves when `raw_codes` is set (tests).
//
// A line that turns out corrupt, or whose chain breaks, counts in `status[0]`; the host then
// shows the CPU decode.

struct Params {
    // first scan line of this batch, scan lines in it
    row_base: u32,
    row_count: u32,
    // macroblocks per scan line, blocks per macroblock (8 for 4:2:2, 6 for 4:2:0)
    nw: u32,
    bpm: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    cheight: u32,
    // chroma macroblock height (16 for 4:2:2, 8 for 4:2:0)
    cmb_h: u32,
    // extra amplitude bits of an AMP_FINDEX symbol (4 at 8 bits, 6 above)
    p_bits: u32,
    // dequantisation: ((4·a + 3)·W·qsf) · inv_4p
    inv_4p: f32,
    // code = clamp(i32(v * scale + bias), 0, out_max)
    scale: f32,
    bias: f32,
    out_max: i32,
    off_y: u32,
    off_cb: u32,
    off_cr: u32,
    pitch_y: u32,
    pitch_c: u32,
    raw_codes: u32,
    // VLC tables in `consts`: primary / secondary offsets and primary index bits
    amp_p: u32,
    amp_s: u32,
    amp_bits: u32,
    run_p: u32,
    run_s: u32,
    run_bits: u32,
    dc_p: u32,
    dc_s: u32,
    dc_bits: u32,
    f16: u32,
    // the VLC tables' words in `consts` (the offsets above are relative to `vlc_base`)
    vlc_base: u32,
    vlc_len: u32,
    // segments of this batch (global indices), their length in bits, the warm-up before a
    // segment's start, how many candidates a `trans` parse may pass without landing
    seg_base: u32,
    seg_count: u32,
    seg_bits: u32,
    warm: u32,
    cap: u32,
    // in `rows`: the line of every segment, and the first segment of every line (and one past the
    // last); in `work`: candidates, transition records and decoder states
    segrow_off: u32,
    rowseg_off: u32,
    w_trans: u32,
    w_state: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the frame's bytes, little-endian words
@group(0) @binding(1) var<storage, read> data: array<u32>;
// 8 words per scan line: byte offset, byte length, mb_row (in its unit), first_line, line_step
@group(0) @binding(2) var<storage, read> rows: array<u32>;
// zigzag [0,64), basis bits [64,128), weights luma [128,191) and chroma [192,255) (index z - 1),
// block layout [256,264) (plane | x << 8 | y << 16), then the VLC tables and the code → half
// table at the offsets in `P`
@group(0) @binding(3) var<storage, read> consts: array<u32>;
@group(0) @binding(4) var<storage, read_write> coeffs: array<f32>;
@group(0) @binding(5) var<storage, read_write> outp: array<u32>;
@group(0) @binding(6) var<storage, read_write> status: array<atomic<u32>>;
// candidates [0, segments), then `TRANS_WORDS` per segment and hypothesis from `w_trans`, then
// `STATE_WORDS` per segment from `w_state` (zeroed by the host: a segment the scan does not reach
// decodes nothing)
@group(0) @binding(7) var<storage, read_write> work: array<u32>;

const BASIS: u32 = 64u;
const WEIGHTS: u32 = 128u;
const LAYOUT: u32 = 256u;
const LINK: u32 = 0x80000000u;
const AMP_FRUN: u32 = 0x100u;
const AMP_FINDEX: u32 = 0x200u;
const AMP_EOB: u32 = 0x400u;
const NONE: u32 = 0xffffffffu;
const TRANS_WORDS: u32 = 6u;
const STATE_WORDS: u32 = 8u;

// ---------------------------------------------------------------------------------------------
// Bit reader over one scan line: absolute bit positions in `data`; bits at or past `r_end` read
// as zero (the CPU reader's view of a scan line as its own byte slice).
// ---------------------------------------------------------------------------------------------

var<private> r_pos: u32;
var<private> r_end: u32;
// the two stream words at `c_idx` (reads only move forward: one load per 32 bits)
var<private> c_idx: u32;
var<private> c0: u32;
var<private> c1: u32;

fn word_be(i: u32) -> u32 {
    let w = data[i];
    return (w << 24u) | ((w & 0xff00u) << 8u) | ((w >> 8u) & 0xff00u) | (w >> 24u);
}

fn reader_at(bit: u32, end: u32) {
    r_pos = bit;
    r_end = end;
    c_idx = bit >> 5u;
    c0 = word_be(c_idx);
    c1 = word_be(c_idx + 1u);
}

// 32 bits starting at bit `pos` (at or after the last one asked for), MSB first.
fn peek32(pos: u32) -> u32 {
    if pos >= r_end {
        return 0u;
    }
    let wi = pos >> 5u;
    if wi != c_idx {
        if wi == c_idx + 1u {
            c0 = c1;
        } else {
            c0 = word_be(wi);
        }
        c1 = word_be(wi + 1u);
        c_idx = wi;
    }
    let sh = pos & 31u;
    var v = c0 << sh;
    if sh != 0u {
        v = v | (c1 >> (32u - sh));
    }
    let left = r_end - pos;
    if left < 32u {
        v = v & ~(0xffffffffu >> left);
    }
    return v;
}

fn read_bits(n: u32) -> u32 {
    if n == 0u {
        return 0u;
    }
    let v = peek32(r_pos) >> (32u - n);
    r_pos = r_pos + n;
    return v;
}

// The tables every symbol and coefficient needs, in workgroup memory (the entropy decoder is one
// long chain of dependent lookups): the VLC tables at the offsets in `P`, then zigzag, weights
// and the block layout from `TAB_ZIGZAG`.
const TAB_LEN: u32 = 3840u;
const TAB_ZIGZAG: u32 = 3584u;
const TAB_WEIGHTS: u32 = 3648u;
const TAB_LAYOUT: u32 = 3776u;
var<workgroup> tab: array<u32, 3840>;

// One symbol of a two-level VLC table (see `filmcraft-dnx` `Reader::vlc`).
fn vlc(primary: u32, secondary: u32, bits: u32) -> u32 {
    let w = peek32(r_pos);
    var e = tab[primary + (w >> (32u - bits))];
    if (e & LINK) != 0u {
        let sb = (e >> 24u) & 0x7fu;
        let off = e & 0xffffffu;
        e = tab[min(secondary + off + ((w << bits) >> (32u - sb)), TAB_ZIGZAG - 1u)];
    }
    r_pos = r_pos + (e >> 16u);
    return e & 0xffffu;
}

// ---------------------------------------------------------------------------------------------
// Entropy decoding
// ---------------------------------------------------------------------------------------------

fn rw(r: u32, k: u32) -> u32 {
    return rows[r * 8u + k];
}

// Coefficient slot base (in floats) of block `b` of macroblock `mb` of local scan line `lr`.
fn slot(lr: u32, mb: u32, b: u32) -> u32 {
    return ((lr * P.nw + mb) * P.bpm + b) * 64u;
}

// One DCT block (8.2.4–8.2.7) into the slot at `base` (only parsed without `write`); false when
// its run overflows the block.
fn decode_block(pred: ptr<function, i32>, chroma: bool, qsf: u32, base: u32, write: bool) -> bool {
    // DC
    let eta = vlc(P.dc_p, P.dc_s, P.dc_bits);
    var eps = 0i;
    if eta != 0u {
        let rho = i32(read_bits(eta));
        if rho >= (1i << (eta - 1u)) {
            eps = rho;
        } else {
            eps = rho + 1i - (1i << eta);
        }
    }
    let dc = *pred + eps;
    *pred = dc;
    if write {
        coeffs[base] = f32(dc);
    }
    // AC
    var w = TAB_WEIGHTS;
    if chroma {
        w = TAB_WEIGHTS + 64u;
    }
    var pos = 0u;
    // every symbol moves `pos` on by at least one: at most 64 of them
    for (var k = 0u; k < 64u; k = k + 1u) {
        let s = vlc(P.amp_p, P.amp_s, P.amp_bits);
        if (s & AMP_EOB) != 0u {
            return true;
        }
        let neg = read_bits(1u) == 1u;
        var a = s & 0xffu;
        if (s & AMP_FINDEX) != 0u {
            a = a + read_bits(P.p_bits) * 64u;
        }
        pos = pos + 1u;
        if (s & AMP_FRUN) != 0u {
            pos = pos + vlc(P.run_p, P.run_s, P.run_bits);
        }
        if pos > 63u {
            return false;
        }
        if !write {
            continue;
        }
        let z = tab[TAB_ZIGZAG + pos];
        // (4·a + 3)·W·qsf rounded once to f32 (both factors are exact), then the power of two
        let mag = f32(4u * a + 3u) * f32(tab[w + z - 1u] * qsf) * P.inv_4p;
        var c = mag;
        if neg {
            c = -mag;
        }
        coeffs[base + z] = c;
    }
    return false;
}

// Fill the workgroup's tables (every invocation of the group, before anything else).
fn load_tables(lid: u32) {
    for (var i = lid; i < TAB_LEN; i = i + 64u) {
        var v = 0u;
        if i < TAB_ZIGZAG {
            if i < P.vlc_len {
                v = consts[P.vlc_base + i];
            }
        } else if i < TAB_WEIGHTS {
            v = consts[i - TAB_ZIGZAG];
        } else if i < TAB_LAYOUT {
            v = consts[WEIGHTS + i - TAB_WEIGHTS];
        } else {
            v = consts[LAYOUT + min(i - TAB_LAYOUT, 7u)];
        }
        tab[i] = v;
    }
    workgroupBarrier();
}

fn invocation(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.y * nwg.x * 64u + gid.x;
}

// The line of global segment `s`, and the first segment of line `r` (`r` + 1: one past its last).
fn seg_row(s: u32) -> u32 {
    return rows[P.segrow_off + s];
}
fn row_seg(r: u32) -> u32 {
    return rows[P.rowseg_off + r];
}

// Bit range of line `r` in `data`.
fn row_start(r: u32) -> u32 {
    return rw(r, 0u) * 8u;
}
fn row_end(r: u32) -> u32 {
    return (rw(r, 0u) + rw(r, 1u)) * 8u;
}

// The colour plane of block `b` of a macroblock.
fn plane_of(b: u32) -> u32 {
    return tab[TAB_LAYOUT + b] & 0xffu;
}

// One DC value (its size VLC and bits), parsed only.
fn skip_dc() {
    let eta = vlc(P.dc_p, P.dc_s, P.dc_bits);
    r_pos = r_pos + eta;
}

// One AC element, parsed only; true at an end of block.
fn skip_ac() -> bool {
    let s = vlc(P.amp_p, P.amp_s, P.amp_bits);
    if (s & AMP_EOB) != 0u {
        return true;
    }
    r_pos = r_pos + 1u;
    if (s & AMP_FINDEX) != 0u {
        r_pos = r_pos + P.p_bits;
    }
    if (s & AMP_FRUN) != 0u {
        vlc(P.run_p, P.run_s, P.run_bits);
    }
    return false;
}

@compute @workgroup_size(64)
fn sync(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lid: u32, @builtin(num_workgroups) nwg: vec3<u32>) {
    load_tables(lid);
    let i = invocation(gid, nwg);
    if i >= P.seg_count {
        return;
    }
    let s = P.seg_base + i;
    let r = seg_row(s);
    let k = s - row_seg(r);
    let start = row_start(r);
    let end = row_end(r);
    if k == 0u {
        // a line starts with its first macroblock
        work[s] = start;
        return;
    }
    let s0 = start + k * P.seg_bits;
    let s1 = min(s0 + P.seg_bits, end);
    reader_at(max(s0 - min(P.warm, s0 - start), start), end);
    var cand = NONE;
    // every element takes a bit at least
    for (var guard = 0u; guard < 2u * (P.warm + P.seg_bits); guard = guard + 1u) {
        if r_pos >= s1 {
            break;
        }
        if skip_ac() {
            if r_pos >= s0 {
                if r_pos < s1 {
                    cand = r_pos;
                }
                break;
            }
            skip_dc();
        }
    }
    work[s] = cand;
}

@compute @workgroup_size(64)
fn trans(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lid: u32, @builtin(num_workgroups) nwg: vec3<u32>) {
    load_tables(lid);
    let i = invocation(gid, nwg);
    if i >= P.seg_count * P.bpm {
        return;
    }
    let s = P.seg_base + i / P.bpm;
    let h = i % P.bpm;
    let out = P.w_trans + (s * P.bpm + h) * TRANS_WORDS;
    let r = seg_row(s);
    let first = row_seg(r);
    let last = row_seg(r + 1u);
    let c = work[s];
    // (a line's first segment starts at its first block)
    if c == NONE || (s == first && h != 0u) {
        work[out] = NONE;
        work[out + 1u] = 0u;
        return;
    }
    let end = row_end(r);
    reader_at(c, end);
    var b = h;
    var nb = 0u;
    var qsf = NONE;
    // DC difference sums of Y, Cb, Cr
    var d0 = 0i;
    var d1 = 0i;
    var d2 = 0i;
    var landed = NONE;
    var j = s + 1u;
    var passed = 0u;
    let total = P.nw * P.bpm;
    loop {
        if nb >= total {
            break;
        }
        if b == 0u {
            qsf = read_bits(11u);
            read_bits(1u);
        }
        let plane = plane_of(b);
        var ok = true;
        if plane == 0u {
            ok = decode_block(&d0, false, 0u, 0u, false);
        } else if plane == 1u {
            ok = decode_block(&d1, true, 0u, 0u, false);
        } else {
            ok = decode_block(&d2, true, 0u, 0u, false);
        }
        if !ok || r_pos > end {
            break;
        }
        nb = nb + 1u;
        b = (b + 1u) % P.bpm;
        // the next candidate at or after this block boundary
        loop {
            if j >= last {
                break;
            }
            let cj = work[j];
            if cj != NONE && cj >= r_pos {
                break;
            }
            if cj != NONE {
                passed = passed + 1u;
            }
            j = j + 1u;
        }
        if j < last && work[j] == r_pos {
            landed = j;
            break;
        }
        if passed > P.cap {
            break;
        }
    }
    work[out] = landed;
    work[out + 1u] = nb;
    work[out + 2u] = qsf;
    work[out + 3u] = bitcast<u32>(d0);
    work[out + 4u] = bitcast<u32>(d1);
    work[out + 5u] = bitcast<u32>(d2);
}

@compute @workgroup_size(64)
fn scan(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let lr = invocation(gid, nwg);
    if lr >= P.row_count {
        return;
    }
    let r = P.row_base + lr;
    let first = row_seg(r);
    let last = row_seg(r + 1u);
    let total = P.nw * P.bpm;
    var k = first;
    var b = 0u;
    var g = 0u;
    var qsf = 0u;
    var p0 = 0i;
    var p1 = 0i;
    var p2 = 0i;
    var done = false;
    for (var guard = first; guard < last; guard = guard + 1u) {
        let t = P.w_trans + (k * P.bpm + b) * TRANS_WORDS;
        let landed = work[t];
        let nb = work[t + 1u];
        let left = total - g;
        let st = P.w_state + k * STATE_WORDS;
        work[st] = g;
        work[st + 1u] = min(nb, left);
        work[st + 2u] = b;
        work[st + 3u] = qsf;
        work[st + 4u] = bitcast<u32>(p0);
        work[st + 5u] = bitcast<u32>(p1);
        work[st + 6u] = bitcast<u32>(p2);
        if nb >= left {
            // the line ends inside this parse
            done = true;
            break;
        }
        if landed == NONE || landed <= k || landed >= last {
            // no true chain onwards: a corrupt line (or one the segments could not resolve)
            break;
        }
        g = g + nb;
        b = (b + nb) % P.bpm;
        if work[t + 2u] != NONE {
            qsf = work[t + 2u];
        }
        p0 = p0 + bitcast<i32>(work[t + 3u]);
        p1 = p1 + bitcast<i32>(work[t + 4u]);
        p2 = p2 + bitcast<i32>(work[t + 5u]);
        k = landed;
    }
    if !done {
        atomicAdd(&status[0], 1u);
    }
}

@compute @workgroup_size(64)
fn decode(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lid: u32, @builtin(num_workgroups) nwg: vec3<u32>) {
    load_tables(lid);
    let i = invocation(gid, nwg);
    if i >= P.seg_count {
        return;
    }
    let s = P.seg_base + i;
    let st = P.w_state + s * STATE_WORDS;
    let n = work[st + 1u];
    if n == 0u {
        return;
    }
    let r = seg_row(s);
    let lr = r - P.row_base;
    let end = row_end(r);
    reader_at(work[s], end);
    var g = work[st];
    var b = work[st + 2u];
    var qsf = work[st + 3u];
    var py = bitcast<i32>(work[st + 4u]);
    var pcb = bitcast<i32>(work[st + 5u]);
    var pcr = bitcast<i32>(work[st + 6u]);
    var bad = false;
    for (var k = 0u; k < n; k = k + 1u) {
        if b == 0u {
            qsf = read_bits(11u);
            // the colour flag (ACF) is meaningful only for RGB streams, which stay on the CPU
            read_bits(1u);
        }
        let plane = plane_of(b);
        let base = slot(lr, g / P.bpm, b);
        var ok = true;
        if plane == 0u {
            ok = decode_block(&py, false, qsf, base, true);
        } else if plane == 1u {
            ok = decode_block(&pcb, true, qsf, base, true);
        } else {
            ok = decode_block(&pcr, true, qsf, base, true);
        }
        g = g + 1u;
        b = (b + 1u) % P.bpm;
        // the CPU decoder checks for an overrun after every macroblock
        if !ok || (b == 0u && r_pos > end) {
            bad = true;
            break;
        }
    }
    if bad {
        atomicAdd(&status[0], 1u);
    }
}

// ---------------------------------------------------------------------------------------------
// IDCT: the CPU's separable f32 inverse DCT (`filmcraft-dnx` `idct8x8`, the same as ProRes's),
// same order of operations: even/odd halves, all-zero rows skipped, DC-only blocks filled
// directly.
// ---------------------------------------------------------------------------------------------

fn basis(u: u32, x: u32) -> f32 {
    return bitcast<f32>(consts[BASIS + u * 8u + x]);
}

var<private> blk: array<f32, 64>;

fn idct8x8() {
    var t: array<array<f32, 8>, 8>;
    var nz: array<bool, 8>;
    var dc_only = true;
    let b00 = basis(0u, 0u);
    for (var r = 0u; r < 8u; r = r + 1u) {
        var ac_zero = true;
        for (var u = 1u; u < 8u; u = u + 1u) {
            if blk[r * 8u + u] != 0.0 {
                ac_zero = false;
            }
        }
        let c0 = blk[r * 8u];
        if ac_zero {
            if c0 != 0.0 {
                let v = c0 * b00;
                for (var x = 0u; x < 8u; x = x + 1u) {
                    t[r][x] = v;
                }
                nz[r] = true;
                dc_only = dc_only && r == 0u;
            }
            continue;
        }
        dc_only = false;
        nz[r] = true;
        let c1 = blk[r * 8u + 1u];
        let c2 = blk[r * 8u + 2u];
        let c3 = blk[r * 8u + 3u];
        let c4 = blk[r * 8u + 4u];
        let c5 = blk[r * 8u + 5u];
        let c6 = blk[r * 8u + 6u];
        let c7 = blk[r * 8u + 7u];
        for (var x = 0u; x < 4u; x = x + 1u) {
            let e = c0 * basis(0u, x) + c2 * basis(2u, x) + c4 * basis(4u, x) + c6 * basis(6u, x);
            let o = c1 * basis(1u, x) + c3 * basis(3u, x) + c5 * basis(5u, x) + c7 * basis(7u, x);
            t[r][x] = e + o;
            t[r][7u - x] = e - o;
        }
    }
    if dc_only {
        let v = t[0][0] * b00;
        for (var i = 0u; i < 64u; i = i + 1u) {
            blk[i] = v;
        }
        return;
    }
    for (var y = 0u; y < 4u; y = y + 1u) {
        var e: array<f32, 8>;
        var o: array<f32, 8>;
        for (var k = 0u; k < 4u; k = k + 1u) {
            let v = 2u * k;
            if nz[v] {
                let b = basis(v, y);
                for (var x = 0u; x < 8u; x = x + 1u) {
                    e[x] = e[x] + t[v][x] * b;
                }
            }
        }
        for (var k = 0u; k < 4u; k = k + 1u) {
            let v = 2u * k + 1u;
            if nz[v] {
                let b = basis(v, y);
                for (var x = 0u; x < 8u; x = x + 1u) {
                    o[x] = o[x] + t[v][x] * b;
                }
            }
        }
        for (var x = 0u; x < 8u; x = x + 1u) {
            blk[y * 8u + x] = e[x] + o[x];
            blk[(7u - y) * 8u + x] = e[x] - o[x];
        }
    }
}

// One output value: a half float (or the code itself) for the packed planes.
fn out_value(code: u32) -> u32 {
    if P.raw_codes != 0u {
        return code;
    }
    return consts[P.f16 + code] & 0xffffu;
}

fn conv(v: f32) -> u32 {
    // floor(v·scale + mid + ½) clamped to the code range; truncation equals floor wherever the
    // result is not clamped to 0
    let t = clamp(v * P.scale + P.bias, -1.0e9, 1.0e9);
    return u32(clamp(i32(t), 0, P.out_max));
}

@compute @workgroup_size(64)
fn idct(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = gid.y * nwg.x * 64u + gid.x;
    let per_row = P.nw * P.bpm;
    let lr = idx / per_row;
    if lr >= P.row_count {
        return;
    }
    let mb = (idx % per_row) / P.bpm;
    let b = idx % P.bpm;
    let base = slot(lr, mb, b);
    for (var i = 0u; i < 64u; i = i + 1u) {
        blk[i] = coeffs[base + i];
    }
    idct8x8();

    let pos = consts[LAYOUT + b];
    let plane = pos & 0xffu;
    let xo = (pos >> 8u) & 0xffu;
    let yo = (pos >> 16u) & 0xffu;
    let r = P.row_base + lr;
    var x0 = mb * 16u + xo;
    var mb_h = 16u;
    var pw = P.width;
    var ph = P.height;
    var off = P.off_y;
    var pitch = P.pitch_y;
    if plane != 0u {
        x0 = mb * 8u + xo;
        mb_h = P.cmb_h;
        pw = P.cwidth;
        ph = P.cheight;
        pitch = P.pitch_c;
        off = P.off_cb;
        if plane == 2u {
            off = P.off_cr;
        }
    }
    if x0 >= pw {
        return;
    }
    let first = rw(r, 3u);
    let step = rw(r, 4u);
    let unit_line = rw(r, 2u) * mb_h + yo;
    for (var yy = 0u; yy < 8u; yy = yy + 1u) {
        let line = first + (unit_line + yy) * step;
        if line >= ph {
            break;
        }
        for (var xx = 0u; xx < 8u; xx = xx + 2u) {
            let x = x0 + xx;
            if x >= pw {
                break;
            }
            let lo = out_value(conv(blk[yy * 8u + xx]));
            var hi = 0u;
            if x + 1u < pw {
                hi = out_value(conv(blk[yy * 8u + xx + 1u]));
            }
            outp[off + line * pitch + (x >> 1u)] = lo | (hi << 16u);
        }
    }
}

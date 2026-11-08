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
// - `trans`: one invocation per segment and hypothesis of the block index in its macroblock at
//   its candidate parses whole blocks from there until it ends a block exactly on a later
//   segment's candidate (or gives up after `cap` candidates): how many blocks that took, the sums
//   of the DC differences per predictor, the last quantiser scale and colour flag read. (Which
//   predictor a block before the first macroblock header counts for depends on the colour flag of
//   an RGB stream's macroblock, which the parse does not know: those sums are kept per plane.)
// - `scan`: one invocation per line follows the true chain from the line's start through these
//   records: every landing of a parse from a true state is a true block boundary, so each segment
//   reached gets the exact decoder state at its candidate (block index, colour flag, DC
//   predictors, quantiser).
// - `decode`: one invocation per reached segment decodes its blocks from that state and writes
//   them dequantised into the coefficient buffer (one slot of 64 floats per block).
// - `idct`: one invocation per 8×8 block runs the inverse DCT (the CPU's separable f32 IDCT, in
//   the same order; the same code as `prores.wgsl`'s), maps the samples to codes and writes them
//   into the output planes.
//
// A line's alpha follows its colour macroblocks: once the colour pass has decoded a line it knows
// where its alpha starts. DCT-coded alpha runs the same passes again over the alpha macroblocks
// (`phase` 1: four blocks each, one DC predictor), from there on the same segment grid. Lossless
// alpha (differential RLE; every macroblock starts its predictors afresh) is parsed by one
// invocation per line (`rle_scan`), which notes where each macroblock starts, and then decoded
// by one invocation per macroblock (`rle_decode`).
//
// RGB streams (4:4:4, see `FrameHeader::rgb_planes`) choose RGB or Y'CbCr per macroblock (ACF,
// recorded in `work`); their IDCT writes codes, and `rgb` then does the CPU's colour conversions
// (`ConvertMacroblockColor` for Y'CbCr macroblocks of mixed streams, then R'G'B' → Y'CbCr) pixel
// by pixel over the whole frame.
//
// Output planes are rows of packed pairs as in `prores.wgsl`: half floats `code / 2^depth`, or the
// codes themselves when `raw_codes` is set (tests).
//
// A line that turns out corrupt, or whose chain breaks, counts in `status[0]`; the host then
// shows the CPU decode. `status[1 + u]` counts the Y'CbCr macroblocks of coding unit `u`.

struct Params {
    // first scan line of this batch, scan lines in it
    row_base: u32,
    row_count: u32,
    // macroblocks per scan line, blocks per macroblock in this phase (4:2:2 8, 4:2:0 6, 4:4:4 12,
    // alpha 4)
    nw: u32,
    bpm: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    cheight: u32,
    // chroma macroblock width and height (8 × 16 for 4:2:2, 8 × 8 for 4:2:0, 16 × 16 for 4:4:4)
    cmb_w: u32,
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
    off_a: u32,
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
    // last); the row record of every frame line
    segrow_off: u32,
    rowseg_off: u32,
    linerec_off: u32,
    // in `work`: transition records, this phase's decoder states, the colour flag of every
    // macroblock, the bit where every line's alpha starts (0: not known)
    w_trans: u32,
    w_state: u32,
    w_acf: u32,
    w_astart: u32,
    // `trans` hypotheses per segment (`bpm`)
    hyps: u32,
    // the colour planes are R'G'B' (4:4:4) / the IDCT writes codes for `rgb` to convert
    rgb: u32,
    codes_out: u32,
    // 0: colour macroblocks, 1: DCT-coded alpha
    phase: u32,
    // colour blocks per macroblock, coefficient slots per macroblock (colour, then alpha), and the
    // first slot of this phase's blocks
    cbpm: u32,
    spm: u32,
    slot_off: u32,
    // coding units (1, or 2 for a field-coded frame) and the macroblocks of each
    units: u32,
    unit_mbs: u32,
    // `rgb`: ConvertMacroblockColor applies (not out-of-band colour) with (kr, kb), and R'G'B' →
    // Y'CbCr with (kr2, kb2); 219/224 and its inverse; 2^(depth - 8)
    to_rgb: u32,
    kr: f32,
    kb: f32,
    kr2: f32,
    kb2: f32,
    c_to_rgb: f32,
    c_to_yuv: f32,
    s8: f32,
    // in `work`: where every lossless alpha macroblock starts
    w_rle: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the frame's bytes, little-endian words
@group(0) @binding(1) var<storage, read> data: array<u32>;
// 8 words per scan line: byte offset, byte length, mb_row (in its unit), first_line, line_step,
// coding unit; then the segment tables and the line → row record table at the offsets in `P`
@group(0) @binding(2) var<storage, read> rows: array<u32>;
// zigzag [0,64), basis bits [64,128), weights luma [128,191) and chroma [192,255) (index z - 1),
// block layouts [256,272) colour and [272,276) alpha (plane | x << 8 | y << 16), then the VLC
// tables and the code → half table at the offsets in `P`
@group(0) @binding(3) var<storage, read> consts: array<u32>;
@group(0) @binding(4) var<storage, read_write> coeffs: array<f32>;
@group(0) @binding(5) var<storage, read_write> outp: array<u32>;
@group(0) @binding(6) var<storage, read_write> status: array<atomic<u32>>;
// candidates [0, segments), then `TRANS_WORDS` per segment and hypothesis from `w_trans`, then
// `STATE_WORDS` per segment from `w_state` (zeroed by the host: a segment the scan does not reach
// decodes nothing), colour flags and alpha starts (zeroed too)
@group(0) @binding(7) var<storage, read_write> work: array<u32>;

const BASIS: u32 = 64u;
const WEIGHTS: u32 = 128u;
const LAYOUT: u32 = 256u;
const LAYOUT_ALPHA: u32 = 272u;
const LINK: u32 = 0x80000000u;
const AMP_FRUN: u32 = 0x100u;
const AMP_FINDEX: u32 = 0x200u;
const AMP_EOB: u32 = 0x400u;
const NONE: u32 = 0xffffffffu;
const TRANS_WORDS: u32 = 13u;
const STATE_WORDS: u32 = 12u;

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
// and the block layouts from `TAB_ZIGZAG`.
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

// Coefficient slot base (in floats) of slot `j` of macroblock `mb` of local scan line `lr`.
fn slot(lr: u32, mb: u32, j: u32) -> u32 {
    return ((lr * P.nw + mb) * P.spm + j) * 64u;
}

// One DCT block (8.2.4–8.2.7) into the slot at `base` (only parsed without `write`), DC predicted
// from `pred`; false when its run overflows the block.
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
            v = consts[LAYOUT + min(i - TAB_LAYOUT, 19u)];
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

// Where this phase's macroblocks of line `r` start (0: not known, the colour pass failed).
fn phase_start(r: u32) -> u32 {
    if P.phase == 0u {
        return row_start(r);
    }
    return work[P.w_astart + r];
}

// This phase's first segment of line `r` (one past its last when the phase does not start).
fn phase_seg(r: u32) -> u32 {
    let first = row_seg(r);
    let last = row_seg(r + 1u);
    let ps = phase_start(r);
    let rs = row_start(r);
    if ps < rs {
        return last;
    }
    return min(first + (ps - rs) / P.seg_bits, last);
}

// The colour plane of block `b` of a macroblock (3: alpha).
fn plane_of(b: u32) -> u32 {
    return tab[TAB_LAYOUT + P.phase * 16u + b] & 0xffu;
}

// The DC predictor of block `b` of a macroblock with colour flag `acf` (R'G'B' macroblocks of an
// RGB stream have their own; alpha has one).
fn pslot(b: u32, acf: u32) -> u32 {
    if P.phase != 0u {
        return 0u;
    }
    let plane = plane_of(b);
    if P.rgb != 0u && acf == 0u {
        return 3u + plane;
    }
    return plane;
}

// A macroblock header: its quantiser scale, and its colour flag where that matters (`acf`).
fn read_header(qsf: ptr<function, u32>, acf: ptr<function, u32>) {
    *qsf = read_bits(11u);
    let f = read_bits(1u);
    if P.rgb != 0u && P.phase == 0u {
        *acf = f;
    }
}

// The predictor sums of `trans` a block before the parse's first macroblock header counts in:
// one per plane, after the six predictors.
fn pre_slot(b: u32) -> u32 {
    if P.phase != 0u {
        return 6u;
    }
    return 6u + plane_of(b);
}

// Parse (and with `write`, decode into `base`) block `b` of a macroblock, DC predicted from
// `(*pred)[k]`.
fn block(pred: ptr<function, array<i32, 9>>, k: u32, b: u32, qsf: u32, base: u32, write: bool) -> bool {
    let plane = plane_of(b);
    let chroma = plane == 1u || plane == 2u;
    var p = (*pred)[k];
    let ok = decode_block(&p, chroma, qsf, base, write);
    (*pred)[k] = p;
    return ok;
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
    let first = phase_seg(r);
    if s < first {
        work[s] = NONE;
        return;
    }
    let start = phase_start(r);
    if s == first {
        // a phase starts with its first macroblock
        work[s] = start;
        return;
    }
    let end = row_end(r);
    let s0 = row_start(r) + (s - row_seg(r)) * P.seg_bits;
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
    if i >= P.seg_count * P.hyps {
        return;
    }
    let s = P.seg_base + i / P.hyps;
    let h = i % P.hyps;
    let out = P.w_trans + (s * P.hyps + h) * TRANS_WORDS;
    let r = seg_row(s);
    let first = phase_seg(r);
    let last = row_seg(r + 1u);
    let c = work[s];
    // (a phase's first segment starts at its first block)
    if c == NONE || (s == first && h != 0u) {
        work[out] = NONE;
        work[out + 1u] = 0u;
        return;
    }
    let end = row_end(r);
    reader_at(c, end);
    var b = h;
    var acf = NONE;
    var nb = 0u;
    var qsf = NONE;
    // DC difference sums per predictor, then per plane before the first header
    var d = array<i32, 9>(0i, 0i, 0i, 0i, 0i, 0i, 0i, 0i, 0i);
    var landed = NONE;
    var j = s + 1u;
    var passed = 0u;
    let total = P.nw * P.bpm;
    loop {
        if nb >= total {
            break;
        }
        if b == 0u {
            var q = 0u;
            var a = 0u;
            read_header(&q, &a);
            qsf = q;
            acf = a;
        }
        var k = pre_slot(b);
        if acf != NONE {
            k = pslot(b, acf);
        }
        if !block(&d, k, b, 0u, 0u, false) || r_pos > end {
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
    work[out + 3u] = acf;
    for (var k = 0u; k < 9u; k = k + 1u) {
        work[out + 4u + k] = bitcast<u32>(d[k]);
    }
}

@compute @workgroup_size(64)
fn scan(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let lr = invocation(gid, nwg);
    if lr >= P.row_count {
        return;
    }
    let r = P.row_base + lr;
    if P.phase != 0u && work[P.w_astart + r] == 0u {
        // the colour pass failed on this line (and said so)
        return;
    }
    let first = phase_seg(r);
    let last = row_seg(r + 1u);
    let total = P.nw * P.bpm;
    var k = first;
    var b = 0u;
    var acf = 0u;
    var g = 0u;
    var qsf = 0u;
    var p = array<i32, 6>(0i, 0i, 0i, 0i, 0i, 0i);
    var done = false;
    for (var guard = first; guard < last; guard = guard + 1u) {
        let t = P.w_trans + (k * P.hyps + b) * TRANS_WORDS;
        let landed = work[t];
        let nb = work[t + 1u];
        let left = total - g;
        let st = P.w_state + k * STATE_WORDS;
        work[st] = g;
        work[st + 1u] = min(nb, left);
        work[st + 2u] = b;
        work[st + 3u] = qsf;
        work[st + 4u] = acf;
        for (var i = 0u; i < 6u; i = i + 1u) {
            work[st + 5u + i] = bitcast<u32>(p[i]);
        }
        if nb >= left {
            // the phase ends inside this parse
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
        // the blocks before the parse's first header belong to the macroblock in progress
        for (var i = 0u; i < 3u; i = i + 1u) {
            var j = i;
            if P.rgb != 0u && P.phase == 0u && acf == 0u {
                j = 3u + i;
            }
            p[j] = p[j] + bitcast<i32>(work[t + 10u + i]);
        }
        for (var i = 0u; i < 6u; i = i + 1u) {
            p[i] = p[i] + bitcast<i32>(work[t + 4u + i]);
        }
        if work[t + 3u] != NONE {
            acf = work[t + 3u];
        }
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
    var acf = work[st + 4u];
    var p = array<i32, 9>(0i, 0i, 0i, 0i, 0i, 0i, 0i, 0i, 0i);
    for (var k = 0u; k < 6u; k = k + 1u) {
        p[k] = bitcast<i32>(work[st + 5u + k]);
    }
    let total = P.nw * P.bpm;
    var bad = false;
    for (var k = 0u; k < n; k = k + 1u) {
        let mb = g / P.bpm;
        if b == 0u {
            read_header(&qsf, &acf);
            if P.rgb != 0u && P.phase == 0u && acf != 0u {
                work[P.w_acf + r * P.nw + mb] = 1u;
                atomicAdd(&status[1u + min(rw(r, 5u), 1u)], 1u);
            }
        }
        let ok = block(&p, pslot(b, acf), b, qsf, slot(lr, mb, P.slot_off + b), true);
        g = g + 1u;
        b = (b + 1u) % P.bpm;
        // the CPU decoder checks for an overrun after every colour macroblock (and after the
        // alpha of a line, which is the same: reads only move forward)
        if !ok || (b == 0u && r_pos > end) {
            bad = true;
            break;
        }
    }
    if bad {
        atomicAdd(&status[0], 1u);
    } else if g == total && P.phase == 0u {
        // the line's alpha (if any) starts where its colour ends
        work[P.w_astart + r] = r_pos;
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
    let per_row = P.nw * P.spm;
    let lr = idx / per_row;
    if lr >= P.row_count {
        return;
    }
    let mb = (idx % per_row) / P.spm;
    let j = idx % P.spm;
    let base = slot(lr, mb, j);
    for (var i = 0u; i < 64u; i = i + 1u) {
        blk[i] = coeffs[base + i];
    }
    idct8x8();

    var pos = 0u;
    if j < P.cbpm {
        pos = consts[LAYOUT + j];
    } else {
        pos = consts[LAYOUT_ALPHA + j - P.cbpm];
    }
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
    // R'G'B' planes stay codes until `rgb` converts them
    var codes = P.codes_out != 0u;
    if plane == 1u || plane == 2u {
        x0 = mb * P.cmb_w + xo;
        mb_h = P.cmb_h;
        pw = P.cwidth;
        ph = P.cheight;
        pitch = P.pitch_c;
        off = P.off_cb;
        if plane == 2u {
            off = P.off_cr;
        }
    } else if plane == 3u {
        off = P.off_a;
        codes = false;
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
            var lo = conv(blk[yy * 8u + xx]);
            var hi = 0u;
            if x + 1u < pw {
                hi = conv(blk[yy * 8u + xx + 1u]);
            }
            if !codes {
                lo = out_value(lo);
                if x + 1u < pw {
                    hi = out_value(hi);
                }
            }
            outp[off + line * pitch + (x >> 1u)] = lo | (hi << 16u);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// R'G'B' streams: the CPU's colour conversions (`filmcraft-dnx` `convert_mb_to_rgb`, then
// `filmcraft-codecs` `dnx_to_video_frame`), same order of operations
// ---------------------------------------------------------------------------------------------

fn floor_code(v: f32) -> u32 {
    return u32(clamp(floor(v), 0.0, f32(P.out_max)));
}

// One pixel's codes (G, B, R or Y', Cb, Cr) → output Y'CbCr codes.
fn convert(px: vec3<u32>, to_rgb: bool, frame_rgb: bool) -> vec3<u32> {
    var c = px;
    let s = P.s8;
    if to_rgb {
        // ConvertMacroblockColor (8.1.1): this macroblock is Y'CbCr in an R'G'B' picture
        let kg = 1.0 - P.kr - P.kb;
        let yy = f32(c.x);
        let cb = f32(c.y) - 128.0 * s;
        let cr = f32(c.z) - 128.0 * s;
        let r = yy + 2.0 * (1.0 - P.kr) * P.c_to_rgb * cr;
        let b = yy + 2.0 * (1.0 - P.kb) * P.c_to_rgb * cb;
        let g = (yy - P.kr * r - P.kb * b) / kg;
        c = vec3<u32>(floor_code(g + 0.5), floor_code(b + 0.5), floor_code(r + 0.5));
    }
    if frame_rgb {
        let kg = 1.0 - P.kr2 - P.kb2;
        let g = f32(c.x);
        let b = f32(c.y);
        let r = f32(c.z);
        let yy = P.kr2 * r + kg * g + P.kb2 * b;
        let u = (b - yy) / (2.0 * (1.0 - P.kb2)) * P.c_to_yuv + 128.0 * s;
        let v = (r - yy) / (2.0 * (1.0 - P.kr2)) * P.c_to_yuv + 128.0 * s;
        c = vec3<u32>(floor_code(yy + 0.5), floor_code(u + 0.5), floor_code(v + 0.5));
    }
    return c;
}

@compute @workgroup_size(64)
fn rgb(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = gid.y * nwg.x * 64u + gid.x;
    let pairs = (P.width + 1u) / 2u;
    let line = idx / pairs;
    if line >= P.height {
        return;
    }
    let x = (idx % pairs) * 2u;
    // a coding unit whose macroblocks are all Y'CbCr is Y'CbCr; so is the frame then
    var ycbcr = array<bool, 2>(false, false);
    var frame_rgb = true;
    for (var u = 0u; u < min(P.units, 2u); u = u + 1u) {
        ycbcr[u] = atomicLoad(&status[1u + u]) == P.unit_mbs;
        frame_rgb = frame_rgb && !ycbcr[u];
    }
    let rec = rows[P.linerec_off + line];
    let unit = min(rw(rec, 5u), 1u);
    let acf = work[P.w_acf + rec * P.nw + x / 16u] != 0u;
    let to_rgb = acf && !ycbcr[unit] && P.to_rgb != 0u;
    let o = line * P.pitch_y + x / 2u;
    let wy = outp[P.off_y + o];
    let wb = outp[P.off_cb + o];
    let wr = outp[P.off_cr + o];
    let lo = convert(vec3<u32>(wy & 0xffffu, wb & 0xffffu, wr & 0xffffu), to_rgb, frame_rgb);
    var hi = vec3<u32>(0u);
    if x + 1u < P.width {
        hi = convert(vec3<u32>(wy >> 16u, wb >> 16u, wr >> 16u), to_rgb, frame_rgb);
        hi = vec3<u32>(out_value(hi.x), out_value(hi.y), out_value(hi.z));
    }
    outp[P.off_y + o] = out_value(lo.x) | (hi.x << 16u);
    outp[P.off_cb + o] = out_value(lo.y) | (hi.y << 16u);
    outp[P.off_cr + o] = out_value(lo.z) | (hi.z << 16u);
}

// ---------------------------------------------------------------------------------------------
// Lossless alpha (§7.3.1.2, §8.3; `filmcraft-dnx` `decode_rle_alpha`)
// ---------------------------------------------------------------------------------------------

var<private> apx: array<u32, 256>;

// One macroblock's 256 samples (serpentine order) from `r_pos`, into `apx` when `write`; false on
// corrupt data, where the CPU decoder fails.
fn rle_mb(write: bool) -> bool {
    let mask = u32(P.out_max);
    let d = countOneBits(mask);
    // RL = 1: RLE coded; RL = 0: 256 raw samples
    if (read_bits(4u) & 1u) == 0u {
        if !write {
            r_pos = r_pos + 256u * d;
            return true;
        }
        for (var count = 0u; count < 256u; count = count + 1u) {
            apx[count] = read_bits(d);
        }
        return true;
    }
    let rice = d / 2u - 2u;
    var d1 = mask >> 1u;
    var d2 = mask >> 1u;
    var count = 0u;
    // every run is a sample long at least
    for (var guard = 0u; guard < 256u; guard = guard + 1u) {
        if count >= 256u {
            break;
        }
        let p = read_bits(1u);
        // Elias gamma: n zeros (at most 8), a one, n bits
        let n = countLeadingZeros(peek32(r_pos));
        if n > 8u {
            // (the CPU decoder stops at the ninth zero, or past the end of the line)
            return false;
        }
        r_pos = r_pos + n + 1u;
        let nrl = (1u << n) + read_bits(n);
        var dcw = 0u;
        if read_bits(1u) == 1u {
            // unary: q ones, then a zero (more than 2^depth ones: corrupt)
            var q = 0u;
            loop {
                let ones = countLeadingZeros(~peek32(r_pos));
                q = q + ones;
                r_pos = r_pos + ones;
                if q > (1u << d) || r_pos > r_end {
                    return false;
                }
                if ones < 32u {
                    break;
                }
            }
            r_pos = r_pos + 1u;
            dcw = (q << rice) + read_bits(rice);
        } else {
            dcw = read_bits(d);
        }
        if count + nrl > 256u {
            return false;
        }
        if !write {
            // parsing only: the predictors do not change what is read
            count = count + nrl;
            continue;
        }
        var diff = dcw >> 1u;
        if (dcw & 1u) == 1u {
            diff = mask - (dcw >> 1u);
        }
        for (var k = 0u; k < nrl; k = k + 1u) {
            var v = (d1 + diff) & mask;
            if p == 1u {
                v = (2u * d1 - d2 + diff) & mask;
            }
            apx[count] = v;
            d2 = d1;
            d1 = v;
            count = count + 1u;
        }
    }
    return count >= 256u;
}

@compute @workgroup_size(64)
fn rle_scan(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let lr = invocation(gid, nwg);
    if lr >= P.row_count {
        return;
    }
    let r = P.row_base + lr;
    let start = work[P.w_astart + r];
    if start == 0u {
        // the colour pass failed on this line (and said so)
        return;
    }
    let end = row_end(r);
    reader_at(start, end);
    var bad = false;
    for (var mb = 0u; mb < P.nw; mb = mb + 1u) {
        work[P.w_rle + r * P.nw + mb] = r_pos;
        if !rle_mb(false) {
            bad = true;
            break;
        }
    }
    // the CPU decoder checks for an overrun after a line's alpha
    if bad || r_pos > end {
        atomicAdd(&status[0], 1u);
    }
}

@compute @workgroup_size(64)
fn rle_decode(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = invocation(gid, nwg);
    let lr = idx / P.nw;
    if lr >= P.row_count {
        return;
    }
    let mb = idx % P.nw;
    let r = P.row_base + lr;
    let start = work[P.w_rle + r * P.nw + mb];
    if work[P.w_astart + r] == 0u || start == 0u {
        return;
    }
    reader_at(start, row_end(r));
    if !rle_mb(true) {
        // (counted by `rle_scan`)
        return;
    }
    let first = rw(r, 3u);
    let step = rw(r, 4u);
    for (var y = 0u; y < 16u; y = y + 1u) {
        let line = first + (rw(r, 2u) * 16u + y) * step;
        if line >= P.height {
            break;
        }
        for (var x = 0u; x < 16u; x = x + 2u) {
            let px = mb * 16u + x;
            if px >= P.width {
                break;
            }
            // serpentine: odd lines right to left
            var i = y * 16u + x;
            var j = i + 1u;
            if (y & 1u) == 1u {
                i = y * 16u + 15u - x;
                j = i - 1u;
            }
            var hi = 0u;
            if px + 1u < P.width {
                hi = out_value(apx[j]);
            }
            outp[P.off_a + line * P.pitch_y + (px >> 1u)] = out_value(apx[i]) | (hi << 16u);
        }
    }
}

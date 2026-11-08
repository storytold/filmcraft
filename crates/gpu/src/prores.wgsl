// ProRes slice decoding (SMPTE RDD 36) in two compute passes, mirroring `filmcraft-prores`'s
// CPU decoder step for step (which stays the reference; tests compare):
//
// - `entropy`: one invocation per slice component (Y, Cb, Cr, and alpha when present) reads its
//   bits, decodes the DC and run/level-coded AC coefficients and writes them dequantised into
//   the coefficient buffer (one slot of 64 floats per block); alpha is lossless and goes
//   straight to its output plane.
// - `idct`: one invocation per 8×8 block runs the inverse DCT (the CPU's separable f32 IDCT, in
//   the same order), maps the samples to codes and writes them into the output planes.
//
// Output planes are rows of packed pairs (two 16-bit values per u32 word, the left sample in the
// low half): half floats `code / 2^depth` (what the compositor samples, through the same
// per-code table as the CPU upload path), or the codes themselves when `raw_codes` is set
// (tests). The host copies them into R16Float textures.
//
// A slice that turns out corrupt counts in `status[0]`; the host then shows the CPU decode.

struct Params {
    // first slice of this batch, slices in it, coefficient slots per slice
    slice_base: u32,
    slice_count: u32,
    max_blocks: u32,
    chroma444: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    interlaced: u32,
    // OutConv: code = clamp(i32(v * scale + bias), out_min, out_max)
    scale: f32,
    bias: f32,
    out_min: i32,
    out_max: i32,
    // output planes: word offsets and row pitches (words) of Y, Cb, Cr, A
    off_y: u32,
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    pitch_y: u32,
    pitch_c: u32,
    // 0 = none, 1 = 8-bit, 2 = 16-bit alpha coding
    alpha: u32,
    depth: u32,
    raw_codes: u32,
    comps: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the frame's bytes, little-endian words
@group(0) @binding(1) var<storage, read> data: array<u32>;
// 16 words per slice: mb_row, mb_x, log2_mbs, parity, qscale, then (offset, length) bytes of Y, Cb, Cr, A
@group(0) @binding(2) var<storage, read> slices: array<u32>;
// qmat luma [0,64), qmat chroma [64,128), scan [128,192), basis bits [192,256),
// codebooks [256,290) (first DC, DC×7, run×16, level×10; rice | exp << 8 | rice_q_max << 16),
// code → half-float table from 320
@group(0) @binding(3) var<storage, read> consts: array<u32>;
@group(0) @binding(4) var<storage, read_write> coeffs: array<f32>;
@group(0) @binding(5) var<storage, read_write> outp: array<u32>;
@group(0) @binding(6) var<storage, read_write> status: array<atomic<u32>>;

const CB_FIRST_DC: u32 = 256u;
const CB_DC: u32 = 257u;
const CB_RUN: u32 = 264u;
const CB_LEVEL: u32 = 280u;
const F16_TABLE: u32 = 320u;
const DC_CTX_INIT: u32 = 5u;
const RUN_CTX_INIT: u32 = 4u;
const LEVEL_CTX_INIT: u32 = 2u;

// ---------------------------------------------------------------------------------------------
// Bit reader over one component: absolute bit positions in `data`; bits at or past `r_end` read
// as zero (the CPU reader's view of a component as its own byte slice).
// ---------------------------------------------------------------------------------------------

var<private> r_pos: u32;
var<private> r_end: u32;
var<private> r_err: bool;

fn word_be(i: u32) -> u32 {
    let w = data[i];
    return (w << 24u) | ((w & 0xff00u) << 8u) | ((w >> 8u) & 0xff00u) | (w >> 24u);
}

// 32 bits starting at bit `pos`, MSB first.
fn peek32(pos: u32) -> u32 {
    if pos >= r_end {
        return 0u;
    }
    let wi = pos >> 5u;
    let sh = pos & 31u;
    var v = word_be(wi) << sh;
    if sh != 0u {
        v = v | (word_be(wi + 1u) >> (32u - sh));
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

fn read_bit() -> bool {
    return read_bits(1u) == 1u;
}

fn bits_left() -> i32 {
    return i32(r_end) - i32(r_pos);
}

// Every remaining bit is zero (the component's padding), or nothing is left.
fn only_padding_left() -> bool {
    let left = bits_left();
    if left > 56 {
        return false;
    }
    if left <= 0 {
        return true;
    }
    let l = u32(left);
    let w0 = peek32(r_pos);
    if l <= 32u {
        return (w0 >> (32u - l)) == 0u;
    }
    return w0 == 0u && (peek32(r_pos + 32u) >> (64u - l)) == 0u;
}

// One codeword of a Rice / Exp-Golomb codebook (see `filmcraft-prores` `Reader::read_cw`).
fn read_cw(cb: u32) -> u32 {
    let rice = cb & 0xffu;
    let ex = (cb >> 8u) & 0xffu;
    let rq = (cb >> 16u) & 0xffu;
    let w0 = peek32(r_pos);
    let w1 = peek32(r_pos + 32u);
    var q = 64u;
    if w0 != 0u {
        q = countLeadingZeros(w0);
    } else if w1 != 0u {
        q = 32u + countLeadingZeros(w1);
    }
    if q <= rq {
        let len = q + 1u + rice;
        var low = 0u;
        if rice != 0u {
            low = peek32(r_pos + q + 1u) >> (32u - rice);
        }
        r_pos = r_pos + len;
        return (q << rice) | low;
    }
    let len = 2u * q - rq + ex;
    if len > 56u {
        r_err = true;
        return 0u;
    }
    // the top `len` bits of the 64-bit window as (hi, lo)
    var hi = 0u;
    var lo = 0u;
    if len <= 32u {
        lo = w0 >> (32u - len);
    } else {
        let sh = 64u - len;
        hi = w0 >> sh;
        lo = (w0 << (32u - sh)) | (w1 >> sh);
    }
    r_pos = r_pos + len;
    // code - 2^exp + ((rice_q_max + 1) << rice), which must fit 32 bits
    let sub = 1u << ex;
    var borrow = 0u;
    if lo < sub {
        borrow = 1u;
    }
    lo = lo - sub;
    if hi < borrow {
        r_err = true;
        return 0u;
    }
    hi = hi - borrow;
    let add = (rq + 1u) << rice;
    let sum = lo + add;
    if sum < lo {
        hi = hi + 1u;
    }
    if hi != 0u {
        r_err = true;
        return 0u;
    }
    return sum;
}

fn codebook(i: u32) -> u32 {
    return consts[i];
}

// ---------------------------------------------------------------------------------------------
// Slices
// ---------------------------------------------------------------------------------------------

fn sl(s: u32, k: u32) -> u32 {
    return slices[s * 16u + k];
}

// Coefficient slot base (in floats) of block `b` of local slice `ls`.
fn slot(ls: u32, b: u32) -> u32 {
    return (ls * P.max_blocks + b) * 64u;
}

// Entropy-decode and dequantise one slice component into consecutive slots from `base`.
fn decode_component(off: u32, len: u32, log2_blocks: u32, qmat: u32, qs: u32, scan: u32, base: u32) {
    r_pos = off * 8u;
    r_end = (off + len) * 8u;
    r_err = false;
    let n = 1u << log2_blocks;

    // DC coefficients
    let code0 = read_cw(codebook(CB_FIRST_DC));
    // to_signed: (c >> 1) ^ -(c & 1)
    var dc = i32(code0 >> 1u) ^ -i32(code0 & 1u);
    let q0 = f32(consts[qmat] * qs);
    coeffs[base] = f32(dc) * q0;
    var code = DC_CTX_INIT;
    var sgn = 0i;
    for (var b = 1u; b < n; b = b + 1u) {
        code = read_cw(codebook(CB_DC + min(code, 6u)));
        if code == 0u {
            sgn = 0i;
        } else {
            sgn = sgn ^ -i32(code & 1u);
        }
        // ((code + 1) >> 1) without overflowing i32
        let mag = i32(code >> 1u) + i32(code & 1u);
        let delta = (mag ^ sgn) - sgn;
        // the CPU sums in 64 bits and clamps to ±2^24; valid streams stay far inside i32
        dc = clamp(clamp(dc, -1073741824, 1073741824) + clamp(delta, -1073741824, 1073741824), -16777216, 16777216);
        coeffs[base + b * 64u] = f32(dc) * q0;
    }

    // AC coefficients, interleaved across blocks: position = scan_index * n + block
    let mask = n - 1u;
    let total = 64u * n;
    var pos = mask;
    var run_ctx = RUN_CTX_INIT;
    var lev_ctx = LEVEL_CTX_INIT;
    var guard = 0u;
    loop {
        if r_err || only_padding_left() {
            break;
        }
        guard = guard + 1u;
        if guard > total {
            r_err = true;
            break;
        }
        let run = read_cw(codebook(CB_RUN + min(run_ctx, 15u)));
        run_ctx = run;
        if run >= total {
            r_err = true;
            break;
        }
        pos = pos + run + 1u;
        if pos >= total {
            r_err = true;
            break;
        }
        var lev = read_cw(codebook(CB_LEVEL + min(lev_ctx, 9u)));
        if lev != 0xffffffffu {
            lev = lev + 1u;
        }
        lev_ctx = lev;
        let neg = read_bit();
        let i = consts[scan + (pos >> log2_blocks)];
        let v = f32(lev) * f32(consts[qmat + i] * qs);
        var c = v;
        if neg {
            c = -v;
        }
        coeffs[base + (pos & mask) * 64u + i] = c;
    }
    if r_err || r_pos > r_end {
        atomicAdd(&status[0], 1u);
    }
}

// One output value: a half float (or the code itself) for the packed planes.
fn out_value(code: u32) -> u32 {
    if P.raw_codes != 0u {
        return code;
    }
    return consts[F16_TABLE + code] & 0xffffu;
}

// Write the low (`x` even) or high half of the word holding sample `x` of plane row `row`.
// Within one invocation's block both halves are written together; see `put_pair`.
fn put_pair(plane_off: u32, pitch: u32, row: u32, x: u32, lo: u32, hi: u32) {
    outp[plane_off + row * pitch + (x >> 1u)] = lo | (hi << 16u);
}

fn alpha_conv(v: u32) -> u32 {
    let depth = P.depth;
    if P.alpha == 1u {
        // 8-bit alpha coding, bit-replicated up to the output depth
        let r = (v << (depth - 8u)) | (v >> min(16u - depth, 8u));
        return r & ((1u << depth) - 1u);
    }
    return v >> (16u - depth);
}

// Lossless alpha of one slice (raster order over its 16 lines), straight into the alpha plane.
fn decode_alpha(s: u32) {
    let off = sl(s, 11u);
    let len = sl(s, 12u);
    r_pos = off * 8u;
    r_end = (off + len) * 8u;
    r_err = false;
    let mbs = 1u << sl(s, 2u);
    let w = mbs * 16u;
    let n = w * 16u;
    var bits = 8u;
    if P.alpha == 2u {
        bits = 16u;
    }
    var short_bits = 4u;
    if bits == 16u {
        short_bits = 7u;
    }
    let vmask = (1u << bits) - 1u;
    var val = vmask;
    var idx = 0u;
    var pending = 0u;
    let mb_row = sl(s, 0u);
    let x0 = sl(s, 1u) * 16u;
    let parity = sl(s, 3u);
    var band_h = 16u;
    var lstep = 1u;
    if P.interlaced != 0u {
        band_h = 32u;
        lstep = 2u;
    }
    let band_y = mb_row * band_h;
    var band_rows = 0u;
    if band_y < P.height {
        band_rows = min(band_h, P.height - band_y);
    }
    var done = false;
    loop {
        if done {
            break;
        }
        // difference-coded values
        loop {
            var d = 0u;
            if read_bit() {
                d = read_bits(bits);
            } else {
                let c = read_bits(short_bits);
                let m = (c + 2u) >> 1u;
                if (c & 1u) == 1u {
                    d = 0u - m;
                } else {
                    d = m;
                }
            }
            val = (val + d) & vmask;
            // emit `val` at idx
            let px = idx % w;
            let py = idx / w;
            let ov = out_value(alpha_conv(val));
            if (px & 1u) == 0u {
                pending = ov;
            }
            let line = py * lstep + parity;
            let x = x0 + px;
            if (px & 1u) == 1u && line < band_rows && x - 1u < P.width {
                put_pair(P.off_a, P.pitch_y, band_y + line, x - 1u, pending, select(0u, ov, x < P.width));
            }
            idx = idx + 1u;
            if idx >= n {
                done = true;
                break;
            }
            if bits_left() <= 0 || !read_bit() {
                break;
            }
        }
        if done {
            break;
        }
        var run = read_bits(4u);
        if run == 0u {
            run = read_bits(11u);
        }
        run = min(run, n - idx);
        let ov = out_value(alpha_conv(val));
        for (var k = 0u; k < run; k = k + 1u) {
            let px = idx % w;
            let py = idx / w;
            if (px & 1u) == 0u {
                pending = ov;
            }
            let line = py * lstep + parity;
            let x = x0 + px;
            if (px & 1u) == 1u && line < band_rows && x - 1u < P.width {
                put_pair(P.off_a, P.pitch_y, band_y + line, x - 1u, pending, select(0u, ov, x < P.width));
            }
            idx = idx + 1u;
        }
        if idx >= n {
            break;
        }
        if bits_left() <= 0 {
            // streams may end early; the remainder repeats the last value
            for (; idx < n; idx = idx + 1u) {
                let px = idx % w;
                let py = idx / w;
                if (px & 1u) == 0u {
                    pending = ov;
                }
                let line = py * lstep + parity;
                let x = x0 + px;
                if (px & 1u) == 1u && line < band_rows && x - 1u < P.width {
                    put_pair(P.off_a, P.pitch_y, band_y + line, x - 1u, pending, select(0u, ov, x < P.width));
                }
            }
            break;
        }
    }
    if bits_left() < -64 {
        atomicAdd(&status[0], 1u);
    }
}

@compute @workgroup_size(64)
fn entropy(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = gid.y * nwg.x * 64u + gid.x;
    let ls = idx / P.comps;
    let comp = idx % P.comps;
    if ls >= P.slice_count {
        return;
    }
    let s = P.slice_base + ls;
    if comp == 3u {
        decode_alpha(s);
        return;
    }
    let log2_mbs = sl(s, 2u);
    let mbs = 1u << log2_mbs;
    let qs = sl(s, 4u);
    let scan = 128u;
    if comp == 0u {
        decode_component(sl(s, 5u), sl(s, 6u), log2_mbs + 2u, 0u, qs, scan, slot(ls, 0u));
        return;
    }
    var cnb = 2u * mbs;
    var clog2 = log2_mbs + 1u;
    if P.chroma444 != 0u {
        cnb = 4u * mbs;
        clog2 = log2_mbs + 2u;
    }
    let first = 4u * mbs + (comp - 1u) * cnb;
    decode_component(sl(s, 5u + 2u * comp), sl(s, 6u + 2u * comp), clog2, 64u, qs, scan, slot(ls, first));
}

// ---------------------------------------------------------------------------------------------
// IDCT: the CPU's separable f32 inverse DCT (`filmcraft-prores` `idct8x8`), same order of
// operations: even/odd halves, all-zero rows skipped, DC-only blocks filled directly.
// ---------------------------------------------------------------------------------------------

fn basis(u: u32, x: u32) -> f32 {
    return bitcast<f32>(consts[192u + u * 8u + x]);
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

fn conv(v: f32) -> u32 {
    // truncation equals floor here: negative values clamp to `out_min` (> 0) either way
    let t = clamp(v * P.scale + P.bias, -1.0e9, 1.0e9);
    return u32(clamp(i32(t), P.out_min, P.out_max));
}

@compute @workgroup_size(64)
fn idct(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = gid.y * nwg.x * 64u + gid.x;
    let ls = idx / P.max_blocks;
    let b = idx % P.max_blocks;
    if ls >= P.slice_count {
        return;
    }
    let s = P.slice_base + ls;
    let mbs = 1u << sl(s, 2u);
    let nluma = 4u * mbs;
    var cnb = 2u * mbs;
    if P.chroma444 != 0u {
        cnb = 4u * mbs;
    }
    if b >= nluma + 2u * cnb {
        return;
    }
    let base = slot(ls, b);
    for (var i = 0u; i < 64u; i = i + 1u) {
        blk[i] = coeffs[base + i];
    }
    idct8x8();

    let mb_x = sl(s, 1u);
    let x0 = mb_x * 16u;
    var bx = 0u;
    var by = 0u;
    var stride = P.width;
    var plane = P.off_y;
    var pitch = P.pitch_y;
    if b < nluma {
        let mb = b >> 2u;
        let i = b & 3u;
        bx = x0 + mb * 16u + (i & 1u) * 8u;
        by = (i >> 1u) * 8u;
    } else {
        var bl = b - nluma;
        plane = P.off_cb;
        if bl >= cnb {
            bl = bl - cnb;
            plane = P.off_cr;
        }
        stride = P.cwidth;
        pitch = P.pitch_c;
        if P.chroma444 != 0u {
            // 4:4:4 chroma blocks run down each 8-wide column pair: TL, BL, TR, BR
            let mb = bl >> 2u;
            let i = bl & 3u;
            bx = x0 + mb * 16u + (i >> 1u) * 8u;
            by = (i & 1u) * 8u;
        } else {
            let mb = bl >> 1u;
            let i = bl & 1u;
            bx = mb_x * 8u + mb * 8u;
            by = i * 8u;
        }
    }
    if bx >= stride {
        return;
    }
    var band_h = 16u;
    var lstep = 1u;
    if P.interlaced != 0u {
        band_h = 32u;
        lstep = 2u;
    }
    let band_y = sl(s, 0u) * band_h;
    if band_y >= P.height {
        return;
    }
    let band_rows = min(band_h, P.height - band_y);
    let parity = sl(s, 3u);
    for (var yy = 0u; yy < 8u; yy = yy + 1u) {
        let line = (by + yy) * lstep + parity;
        if line >= band_rows {
            break;
        }
        for (var xx = 0u; xx < 8u; xx = xx + 2u) {
            let x = bx + xx;
            if x >= stride {
                break;
            }
            let lo = out_value(conv(blk[yy * 8u + xx]));
            var hi = 0u;
            if x + 1u < stride {
                hi = out_value(conv(blk[yy * 8u + xx + 1u]));
            }
            put_pair(plane, pitch, band_y + line, x, lo, hi);
        }
    }
}

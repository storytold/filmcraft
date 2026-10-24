//! Hand-built bitstreams for coding tools libx265 never produces: tiles (uniform and explicit spacing,
//! with and without loop filtering across tile boundaries), PCM (with and without in-loop filtering),
//! multiple slices, dependent slice segments, long-term reference pictures and reference list
//! modification. A small CABAC encoder (9.3.5, informative) writes the slice data. The expected output
//! is known exactly where the in-loop filters leave the samples alone, and every stream is also checked
//! against ffmpeg (when present).

use crate::Decoder;
use crate::cabac::{Contexts, NEXT_STATE, RANGE_TAB_LPS, init_contexts};
use crate::spec_tables::*;
use filmcraft_bitstream::{BitWriter, escape_rbsp};

const W: usize = 96;
const H: usize = 64;
const CTB: usize = 16;
const WC: usize = W / CTB;
const HC: usize = H / CTB;

/// CABAC encoder.
struct Enc {
    w: BitWriter,
    low: u32,
    range: u32,
    outstanding: u32,
    first: bool,
    ctx: Contexts,
}

impl Enc {
    fn new(w: BitWriter, qp: i32, init_type: usize) -> Self {
        let mut ctx: Contexts = [0u8; NUM_CTX.next_power_of_two()];
        init_contexts(&mut ctx, qp, init_type);
        Enc { w, low: 0, range: 510, outstanding: 0, first: true, ctx }
    }
    fn reset_engine(&mut self) {
        self.low = 0;
        self.range = 510;
        self.outstanding = 0;
        self.first = true;
    }
    fn put(&mut self, b: u32) {
        if self.first {
            self.first = false;
        } else {
            self.w.write_bits(b, 1);
        }
        while self.outstanding > 0 {
            self.w.write_bits(1 - b, 1);
            self.outstanding -= 1;
        }
    }
    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put(1);
            } else {
                self.low -= 256;
                self.outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }
    fn decision(&mut self, ctx: usize, bin: u32) {
        let s = self.ctx[ctx] as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps = RANGE_TAB_LPS[s >> 1][q] as u32;
        self.range -= lps;
        if bin != (s & 1) as u32 {
            self.low += self.range;
            self.range = lps;
            self.ctx[ctx] = NEXT_STATE[s][1];
        } else {
            self.ctx[ctx] = NEXT_STATE[s][0];
        }
        self.renorm();
    }
    /// Terminating bin; a 1 flushes the engine (9.3.5.6) — its last written bit is the stop /
    /// alignment bit.
    fn terminate(&mut self, bin: u32) {
        self.range -= 2;
        if bin != 0 {
            self.low += self.range;
            self.range = 2;
            self.renorm();
            self.put((self.low >> 9) & 1);
            self.w.write_bits(((self.low >> 7) & 3) | 1, 2);
        } else {
            self.renorm();
        }
    }
}

/// Deterministic test content.
fn pattern(pic: u32, c: usize, x: usize, y: usize) -> u8 {
    let v = (x * (3 + c) + y * (5 + pic as usize) + ((x ^ y) & 7) * 9 + pic as usize * 37 + c * 50) % 200;
    (v + 20) as u8
}

struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn content(pic: u32) -> Planes {
    let mut p = Planes { y: vec![0; W * H], u: vec![0; W * H / 4], v: vec![0; W * H / 4] };
    for y in 0..H {
        for x in 0..W {
            p.y[y * W + x] = pattern(pic, 0, x, y);
        }
    }
    for y in 0..H / 2 {
        for x in 0..W / 2 {
            p.u[y * W / 2 + x] = pattern(pic, 1, x, y);
            p.v[y * W / 2 + x] = pattern(pic, 2, x, y);
        }
    }
    p
}

fn nal(t: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, t << 1, 1];
    out.extend(escape_rbsp(rbsp));
    out
}

fn ptl(w: &mut BitWriter) {
    w.write_bits(0, 2); // profile_space
    w.write_bits(0, 1); // tier
    w.write_bits(1, 5); // Main
    w.write_bits(0x6000_0000, 32); // compatibility flags 1, 2
    w.write_bits(0b1001, 4); // progressive, interlaced, non_packed, frame_only
    w.write_bits(0, 32);
    w.write_bits(0, 11);
    w.write_bits(0, 1);
    w.write_bits(93, 8); // level 3.1
}

fn vps() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0, 4);
    w.write_bits(3, 2); // base layer internal / available
    w.write_bits(0, 6);
    w.write_bits(0, 3);
    w.write_bits(1, 1);
    w.write_bits(0xffff, 16);
    ptl(&mut w);
    w.write_bits(1, 1);
    w.write_ue(4);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bits(0, 6);
    w.write_ue(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.rbsp_trailing();
    nal(32, &w.finish())
}

fn sps(pcm_loop_filter_disabled: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0, 4);
    w.write_bits(0, 3);
    w.write_bits(1, 1);
    ptl(&mut w);
    w.write_ue(0); // sps id
    w.write_ue(1); // 4:2:0
    w.write_ue(W as u32);
    w.write_ue(H as u32);
    w.write_bits(0, 1); // conformance window
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(4); // log2_max_poc_lsb = 8
    w.write_bits(1, 1);
    w.write_ue(4); // max_dec_pic_buffering_minus1
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(0); // min CB 8
    w.write_ue(1); // CTB 16
    w.write_ue(0); // min TB 4
    w.write_ue(2); // max TB 16
    w.write_ue(0);
    w.write_ue(0);
    w.write_bits(0, 1); // scaling lists
    w.write_bits(0, 1); // amp
    w.write_bits(0, 1); // sao
    w.write_bits(1, 1); // pcm
    w.write_bits(7, 4);
    w.write_bits(7, 4);
    w.write_ue(0); // min PCM 8
    w.write_ue(1); // max PCM 16
    w.write_bits(pcm_loop_filter_disabled as u32, 1);
    w.write_ue(0); // num_short_term_ref_pic_sets
    w.write_bits(1, 1); // long_term_ref_pics_present
    w.write_ue(0);
    w.write_bits(0, 1); // temporal mvp
    w.write_bits(0, 1); // strong intra smoothing
    w.write_bits(0, 1); // vui
    w.write_bits(0, 1); // extensions
    w.rbsp_trailing();
    nal(33, &w.finish())
}

/// Tile layout: None = no tiles; Some((column widths, row heights, uniform, across)).
type Tiles = Option<(Vec<usize>, Vec<usize>, bool, bool)>;

fn pps(id: u32, tiles: &Tiles, lists_mod: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_ue(id);
    w.write_ue(0);
    w.write_bits(1, 1); // dependent slice segments enabled
    w.write_bits(0, 1);
    w.write_bits(0, 3);
    w.write_bits(0, 1); // sign hiding
    w.write_bits(0, 1); // cabac_init_present
    w.write_ue(0);
    w.write_ue(0);
    w.write_se(0); // init_qp 26
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1); // cu_qp_delta
    w.write_se(0);
    w.write_se(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1); // transquant bypass
    w.write_bits(tiles.is_some() as u32, 1);
    w.write_bits(0, 1); // entropy coding sync
    if let Some((cols, rows, uniform, across)) = tiles {
        w.write_ue(cols.len() as u32 - 1);
        w.write_ue(rows.len() as u32 - 1);
        w.write_bits(*uniform as u32, 1);
        if !uniform {
            for c in &cols[..cols.len() - 1] {
                w.write_ue(*c as u32 - 1);
            }
            for r in &rows[..rows.len() - 1] {
                w.write_ue(*r as u32 - 1);
            }
        }
        w.write_bits(*across as u32, 1);
    }
    w.write_bits(1, 1); // loop filter across slices
    w.write_bits(0, 1); // deblocking control
    w.write_bits(0, 1); // scaling list
    w.write_bits(lists_mod as u32, 1);
    w.write_ue(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.rbsp_trailing();
    nal(34, &w.finish())
}

/// Tile scan order of CTB raster addresses and the tile index of each CTB (6.5.1).
fn tile_scan(tiles: &Tiles) -> (Vec<usize>, Vec<usize>) {
    let (cols, rows) = match tiles {
        Some((c, r, _, _)) => (c.clone(), r.clone()),
        None => (vec![WC], vec![HC]),
    };
    let mut order = Vec::new();
    let mut tile = vec![0; WC * HC];
    let mut t = 0;
    let mut y0 = 0;
    for &rh in &rows {
        let mut x0 = 0;
        for &cw in &cols {
            for y in y0..y0 + rh {
                for x in x0..x0 + cw {
                    order.push(y * WC + x);
                    tile[y * WC + x] = t;
                }
            }
            t += 1;
            x0 += cw;
        }
        y0 += rh;
    }
    (order, tile)
}

#[derive(Clone, Copy, PartialEq)]
enum Cu {
    Pcm,
    Skip,
}

/// One slice segment: first CTB index in tile scan, number of CTBs, dependent flag.
struct Segment {
    start: usize,
    len: usize,
    dependent: bool,
}

struct PicDesc {
    nal_type: u8,
    poc: u32,
    intra: bool,
    cu: Cu,
    pps_id: u32,
    /// st RPS: (delta, used) negative pictures.
    st: Vec<(i32, bool)>,
    /// long-term: (poc lsb, used).
    lt: Vec<(u32, bool)>,
    num_ref: u32,
    list_entry: Option<Vec<u32>>,
    segments: Vec<Segment>,
}

/// Write the slice segment NAL units of a picture.
fn encode_picture(d: &PicDesc, tiles: &Tiles, pcm_content: &Planes, out: &mut Vec<u8>, saved: &mut Option<Contexts>) {
    let (order, tile_of) = tile_scan(tiles);
    let qp = 26;
    let init_type = if d.intra { 0 } else { 1 };
    for seg in &d.segments {
        // slice data first (entry points go into the header)
        let mut e = Enc::new(BitWriter::new(), qp, init_type);
        if seg.dependent {
            e.ctx = saved.expect("contexts of the previous segment");
        }
        let mut starts = vec![0usize];
        for k in seg.start..seg.start + seg.len {
            let rs = order[k];
            let (rx, ry) = (rs % WC, rs / WC);
            // the first CTU of a tile always starts from initialised contexts (9.3.1)
            if k > 0 && tile_of[rs] != tile_of[order[k - 1]] {
                init_contexts(&mut e.ctx, qp, init_type);
            }
            e.decision(SPLIT_CU, 0);
            match d.cu {
                Cu::Pcm => {
                    if !d.intra {
                        e.decision(CU_SKIP, 0);
                        e.decision(PRED_MODE, 1);
                    }
                    e.terminate(1); // pcm_flag
                    e.w.align_zero();
                    for y in 0..CTB {
                        for x in 0..CTB {
                            e.w.write_bits(pcm_content.y[(ry * CTB + y) * W + rx * CTB + x] as u32, 8);
                        }
                    }
                    for plane in [&pcm_content.u, &pcm_content.v] {
                        for y in 0..CTB / 2 {
                            for x in 0..CTB / 2 {
                                e.w.write_bits(plane[(ry * CTB / 2 + y) * W / 2 + rx * CTB / 2 + x] as u32, 8);
                            }
                        }
                    }
                    e.reset_engine();
                }
                Cu::Skip => {
                    // all CUs are skipped: ctxInc = available left + available above (single tile / slice)
                    let inc = (rx > 0) as usize + (ry > 0) as usize;
                    e.decision(CU_SKIP + inc, 1);
                }
            }
            let last = k + 1 == seg.start + seg.len;
            e.terminate(last as u32);
            if last {
                e.w.align_zero();
            } else if tile_of[order[k + 1]] != tile_of[rs] {
                e.terminate(1); // end_of_subset_one_bit
                e.w.align_zero();
                starts.push(e.w.bit_len() / 8);
                e.reset_engine();
            }
        }
        *saved = Some(e.ctx);
        let data = e.w.finish();
        // header, iterating until the entry point offsets (measured on the escaped NAL) are stable
        let mut offsets: Vec<u32> = starts.windows(2).map(|s| (s[1] - s[0]) as u32).collect();
        loop {
            let mut w = BitWriter::new();
            let first = seg.start == 0;
            w.write_bit(first);
            if (16..=23).contains(&d.nal_type) {
                w.write_bit(false);
            }
            w.write_ue(d.pps_id);
            if !first {
                w.write_bit(seg.dependent);
                w.write_bits(order[seg.start] as u32, 5); // Ceil(Log2(24)) bits
            }
            if !seg.dependent {
                w.write_ue(if d.intra { 2 } else { 1 });
                if d.nal_type != 19 && d.nal_type != 20 {
                    w.write_bits(d.poc & 255, 8);
                    w.write_bit(false); // short_term_ref_pic_set_sps_flag
                    w.write_ue(d.st.len() as u32);
                    w.write_ue(0);
                    let mut prev = 0;
                    for &(dp, used) in &d.st {
                        w.write_ue((prev - dp - 1) as u32);
                        w.write_bit(used);
                        prev = dp;
                    }
                    w.write_ue(d.lt.len() as u32);
                    for &(lsb, used) in &d.lt {
                        w.write_bits(lsb, 8);
                        w.write_bit(used);
                        w.write_bit(false);
                    }
                }
                if !d.intra {
                    w.write_bit(true); // num_ref_idx_active_override_flag
                    w.write_ue(d.num_ref - 1);
                    let total = d.st.iter().filter(|s| s.1).count() + d.lt.iter().filter(|l| l.1).count();
                    // PPS 1 has lists_modification_present_flag set
                    if d.list_entry.is_none() && d.pps_id == 1 && total > 1 {
                        w.write_bit(false);
                    }
                    if let Some(e) = &d.list_entry {
                        w.write_bit(true);
                        let bits = 32 - (total as u32 - 1).leading_zeros();
                        for &v in e {
                            w.write_bits(v, bits);
                        }
                    }
                    w.write_ue(4); // five_minus_max_num_merge_cand
                }
                w.write_se(0); // slice_qp_delta
                w.write_bit(true); // slice_loop_filter_across_slices_enabled_flag
            }
            if tiles.is_some() {
                w.write_ue(offsets.len() as u32);
                if !offsets.is_empty() {
                    w.write_ue(31);
                    for &o in &offsets {
                        w.write_bits(o - 1, 32);
                    }
                }
            }
            w.write_bit(true);
            w.align_zero();
            let mut rbsp = w.finish();
            let hdr_len = rbsp.len();
            rbsp.extend_from_slice(&data);
            // escaped position of every substream start
            let esc_pos = |idx: usize| escape_rbsp(&rbsp[..idx]).len();
            let s: Vec<usize> = starts.iter().map(|&s| esc_pos(hdr_len + s)).collect();
            let new: Vec<u32> = s.windows(2).map(|p| (p[1] - p[0]) as u32).collect();
            if new == offsets {
                out.extend(nal(d.nal_type, &rbsp));
                break;
            }
            offsets = new;
        }
    }
}

fn whole(n: usize) -> Vec<Segment> {
    vec![Segment { start: 0, len: n, dependent: false }]
}

/// Encode the test sequence; returns the stream and the exact expected pictures (when the PCM samples
/// are not filtered).
fn sequence(tiles: &Tiles, pcm_lf_disabled: bool, segments: Vec<Segment>) -> (Vec<u8>, Vec<Planes>) {
    let mut s = Vec::new();
    s.extend(vps());
    s.extend(sps(pcm_lf_disabled));
    s.extend(pps(0, tiles, false));
    s.extend(pps(1, &None, true));
    let n = WC * HC;
    let mut saved = None;
    let c0 = content(0);
    let c1 = content(1);
    // 0: IDR, PCM, tiles / slices / dependent segments
    let p0 = PicDesc { nal_type: 19, poc: 0, intra: true, cu: Cu::Pcm, pps_id: 0, st: vec![], lt: vec![], num_ref: 0, list_entry: None, segments };
    encode_picture(&p0, tiles, &c0, &mut s, &mut saved);
    // 1: intra TRAIL_R keeping picture 0 as a long-term reference (not used by this picture)
    let p1 = PicDesc {
        nal_type: 1,
        poc: 1,
        intra: true,
        cu: Cu::Pcm,
        pps_id: 1,
        st: vec![],
        lt: vec![(0, false)],
        num_ref: 0,
        list_entry: None,
        segments: whole(n),
    };
    encode_picture(&p1, &None, &c1, &mut s, &mut saved);
    // 2: P, all skip, RefPicList0 = { LT picture 0 } -> a copy of picture 0
    let p2 = PicDesc {
        nal_type: 1,
        poc: 2,
        intra: false,
        cu: Cu::Skip,
        pps_id: 1,
        st: vec![(-1, false)],
        lt: vec![(0, true)],
        num_ref: 1,
        list_entry: None,
        segments: whole(n),
    };
    encode_picture(&p2, &None, &c1, &mut s, &mut saved);
    // 3: P, all skip, list modification puts the long-term picture first -> picture 0 again
    let p3 = PicDesc {
        nal_type: 1,
        poc: 3,
        intra: false,
        cu: Cu::Skip,
        pps_id: 1,
        st: vec![(-1, true), (-2, true)],
        lt: vec![(0, true)],
        num_ref: 3,
        list_entry: Some(vec![2, 0, 1]),
        segments: whole(n),
    };
    encode_picture(&p3, &None, &c1, &mut s, &mut saved);
    // 4: P, all skip, default list order: refIdx 0 = picture 3 (a copy of picture 0)
    let p4 = PicDesc {
        nal_type: 1,
        poc: 4,
        intra: false,
        cu: Cu::Skip,
        pps_id: 1,
        st: vec![(-1, true), (-3, true)],
        lt: vec![(0, true)],
        num_ref: 2,
        list_entry: None,
        segments: whole(n),
    };
    encode_picture(&p4, &None, &c1, &mut s, &mut saved);
    let expect = vec![content(0), content(1), content(0), content(0), content(0)];
    (s, expect)
}

fn decode_all(stream: &[u8], threads: usize) -> Vec<crate::Picture> {
    decode_all_cfg(stream, threads, None).0
}

/// Decode with decoding inside pictures forced on / off (None: the decoder's choice).
fn decode_all_cfg(stream: &[u8], threads: usize, intra: Option<bool>) -> (Vec<crate::Picture>, crate::DecodeStats) {
    let mut dec = Decoder::with_threads(threads);
    dec.set_intra_picture_parallel(intra);
    let mut pics = dec.decode(stream, 0).expect("decode");
    pics.extend(dec.flush());
    assert!(dec.take_error().is_none());
    (pics, dec.stats())
}

fn as_u8(p: &crate::Plane) -> &[u8] {
    p.as_u8().expect("8-bit output")
}

/// Compare with ffmpeg's decode of the same stream (skipped when ffmpeg is absent).
fn check_ffmpeg(name: &str, stream: &[u8], pics: &[crate::Picture]) {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip(name) else { return };
    let dir = filmcraft_testkit::fixtures_dir("hevc");
    let src = filmcraft_testkit::temp_path(&dir.join(format!("synth_{name}.hevc")));
    let yuv = filmcraft_testkit::temp_path(&dir.join(format!("synth_{name}.yuv")));
    std::fs::write(&src, stream).unwrap();
    let st = std::process::Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&src)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&yuv)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg failed on {name}");
    let r = std::fs::read(&yuv).unwrap();
    let fs = W * H * 3 / 2;
    assert_eq!(r.len(), fs * pics.len(), "{name}: ffmpeg decoded {} frames", r.len() / fs);
    for (i, p) in pics.iter().enumerate() {
        let f = &r[i * fs..(i + 1) * fs];
        let mut ours = as_u8(&p.y).to_vec();
        ours.extend_from_slice(as_u8(&p.u));
        ours.extend_from_slice(as_u8(&p.v));
        if ours != f {
            let k = ours.iter().zip(f).position(|(a, b)| a != b).unwrap();
            panic!("{name}: picture {i} differs from ffmpeg at byte {k}: {} vs {}", ours[k], f[k]);
        }
    }
    let _ = std::fs::remove_file(&yuv);
    let _ = std::fs::remove_file(&src);
}

fn run(name: &str, tiles: Tiles, pcm_lf_disabled: bool, segments: Vec<Segment>) {
    let (stream, expect) = sequence(&tiles, pcm_lf_disabled, segments);
    if let Ok(d) = std::env::var("HEVC_SYNTH_DUMP") {
        std::fs::write(format!("{d}/{name}.hevc"), &stream).unwrap();
    }
    for threads in [1, 4] {
        let pics = decode_all(&stream, threads);
        assert_eq!(pics.len(), expect.len(), "{name}");
        for (i, (p, e)) in pics.iter().zip(&expect).enumerate() {
            assert_eq!(p.poc, i as i32);
            if pcm_lf_disabled {
                assert!(as_u8(&p.y) == &e.y[..], "{name}: picture {i} luma");
                assert!(as_u8(&p.u) == &e.u[..] && as_u8(&p.v) == &e.v[..], "{name}: picture {i} chroma");
            }
        }
        if threads == 1 {
            check_ffmpeg(name, &stream, &pics);
        }
    }
    // tiles on several threads, or (no tiles) filters running next to the parsing thread: the
    // same pictures as one thread
    let serial = decode_all(&stream, 1);
    for threads in [2, 3, 8] {
        let (pics, stats) = decode_all_cfg(&stream, threads, Some(true));
        // (without the thread pool everything is decoded on the calling thread)
        assert!(stats.parallel_pictures > 0 || cfg!(not(feature = "threads")), "{name}: no picture decoded in parallel");
        for (i, (a, b)) in pics.iter().zip(&serial).enumerate() {
            assert!(a.y == b.y && a.u == b.u && a.v == b.v, "{name} (threads {threads}): picture {i} differs from one thread");
        }
        assert_eq!(pics.len(), serial.len(), "{name}");
    }
}

#[test]
fn pcm_long_term_refs_list_modification() {
    run("pcm_lt", None, true, whole(WC * HC));
}

#[test]
fn pcm_deblocked_single_tile() {
    run("pcm_dbk", None, false, whole(WC * HC));
}

#[test]
fn tiles_uniform_exact() {
    run("tiles_uniform", Some((vec![2, 2, 2], vec![2, 2], true, true)), true, whole(WC * HC));
}

#[test]
fn tiles_explicit_filter_across() {
    run("tiles_across", Some((vec![1, 3, 2], vec![1, 3], false, true)), false, whole(WC * HC));
}

#[test]
fn tiles_explicit_no_filter_across() {
    run("tiles_no_across", Some((vec![2, 3, 1], vec![3, 1], false, false)), false, whole(WC * HC));
}

#[test]
fn tiles_slices_and_dependent_segments() {
    // tiles 2x2 (3x2 CTBs each): slice 0 = tiles 0-1, slice 1 = tiles 2-3 split into three segments,
    // the dependent ones starting inside a tile (context restore) and at a tile start
    let segs = vec![
        Segment { start: 0, len: 12, dependent: false },
        Segment { start: 12, len: 2, dependent: false },
        Segment { start: 14, len: 4, dependent: true },
        Segment { start: 18, len: 6, dependent: true },
    ];
    run("tiles_slices", Some((vec![3, 3], vec![2, 2], true, false)), false, segs);
}

#[test]
fn slices_without_tiles() {
    let segs =
        vec![Segment { start: 0, len: 7, dependent: false }, Segment { start: 7, len: 9, dependent: true }, Segment { start: 16, len: 8, dependent: false }];
    run("slices", None, false, segs);
}

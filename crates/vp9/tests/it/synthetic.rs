//! Streams libvpx (as driven by ffmpeg) does not produce, built by rewriting the uncompressed
//! headers of libvpx-vp9 output, all checked against ffmpeg's decoder:
//! - reference scaling: inter frames of a stream encoded at another size are spliced after a key
//!   frame, with their frame size coded explicitly, so they predict from differently sized
//!   references (down- and up-scaling);
//! - intra-only frames (a key frame rewritten as a hidden intra-only frame) shown with
//!   show_existing_frame;
//! - show_existing_frame of hidden alt-ref frames and of arbitrary slots.

use crate::common;

use std::path::Path;

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn f(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | ((self.d[self.pos / 8] >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        v
    }
}

#[derive(Default)]
struct W {
    bits: Vec<bool>,
}

impl W {
    fn put(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            self.bits.push((v >> i) & 1 == 1);
        }
    }
    fn copy(&mut self, d: &[u8], from: usize, to: usize) {
        let mut b = Bits { d, pos: from };
        for _ in from..to {
            self.bits.push(b.f(1) == 1);
        }
    }
    fn bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.bits.len().div_ceil(8)];
        for (i, &b) in self.bits.iter().enumerate() {
            if b {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }
}

/// Parse the header fields from refresh_frame_context to header_size_in_bytes (profile 0
/// layouts); returns the bit position after header_size_in_bytes.
fn parse_tail(b: &mut Bits, error_res: bool, width: u32) -> usize {
    if !error_res {
        b.f(2);
    }
    b.f(2); // frame_context_idx
    b.f(6);
    b.f(3);
    if b.f(1) == 1 && b.f(1) == 1 {
        for _ in 0..6 {
            if b.f(1) == 1 {
                b.f(7);
            }
        }
    }
    b.f(8);
    for _ in 0..3 {
        if b.f(1) == 1 {
            b.f(5);
        }
    }
    if b.f(1) == 1 {
        if b.f(1) == 1 {
            for _ in 0..7 {
                if b.f(1) == 1 {
                    b.f(8);
                }
            }
            if b.f(1) == 1 {
                for _ in 0..3 {
                    if b.f(1) == 1 {
                        b.f(8);
                    }
                }
            }
        }
        if b.f(1) == 1 {
            b.f(1);
            for _ in 0..8 {
                for (bits, signed) in [(8, true), (6, true), (2, false), (0, false)] {
                    if b.f(1) == 1 {
                        b.f(bits);
                        if signed {
                            b.f(1);
                        }
                    }
                }
            }
        }
    }
    let sb_cols = (width.div_ceil(8)).div_ceil(8);
    let mut min_log2 = 0;
    while (64 << min_log2) < sb_cols {
        min_log2 += 1;
    }
    let mut max_log2 = 1;
    while (sb_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    max_log2 -= 1;
    let mut l = min_log2;
    while l < max_log2 {
        if b.f(1) == 1 {
            l += 1;
        } else {
            break;
        }
    }
    if b.f(1) == 1 {
        b.f(1);
    }
    b.f(16);
    b.pos
}

/// Rewrite a profile 0 inter frame so that its frame size (`w` x `h`) is coded explicitly.
fn explicit_size(frame: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut b = Bits { d: frame, pos: 0 };
    assert_eq!(b.f(2), 2);
    assert_eq!(b.f(2), 0, "profile 0 only");
    assert_eq!(b.f(1), 0, "show_existing");
    assert_eq!(b.f(1), 1, "inter frame expected");
    let show = b.f(1) == 1;
    let err = b.f(1) == 1;
    if !show {
        assert_eq!(b.f(1), 0, "intra-only");
    }
    if !err {
        b.f(2);
    }
    b.f(8 + 12);
    let found_at = b.pos;
    let mut found = false;
    for _ in 0..3 {
        if b.f(1) == 1 {
            found = true;
            break;
        }
    }
    if !found {
        b.f(32);
    }
    let after_size = b.pos;
    // render_size, allow_high_precision_mv, interpolation filter
    if b.f(1) == 1 {
        b.f(32);
    }
    b.f(1);
    if b.f(1) == 0 {
        b.f(2);
    }
    let end = parse_tail(&mut b, err, w);
    let mut o = W::default();
    o.copy(frame, 0, found_at);
    o.put(0, 3);
    o.put(w - 1, 16);
    o.put(h - 1, 16);
    o.copy(frame, after_size, end);
    let mut out = o.bytes();
    out.extend_from_slice(&frame[end.div_ceil(8)..]);
    out
}

/// Rewrite a profile 0 key frame as a hidden intra-only frame refreshing `refresh` slots.
fn key_to_intra_only(frame: &[u8], refresh: u8, reset: u32) -> Vec<u8> {
    let mut b = Bits { d: frame, pos: 0 };
    assert_eq!(b.f(2), 2);
    assert_eq!(b.f(2), 0);
    assert_eq!(b.f(1), 0);
    assert_eq!(b.f(1), 0, "key frame expected");
    b.f(1); // show_frame
    let err = b.f(1) == 1;
    assert_eq!(b.f(24), 0x498342);
    b.f(4); // color_space, color_range
    let size_at = b.pos;
    let w = b.f(16) + 1;
    b.f(16);
    if b.f(1) == 1 {
        b.f(32);
    }
    let after_render = b.pos;
    let end = parse_tail(&mut b, err, w);
    let mut o = W::default();
    o.put(2, 2);
    o.put(0, 2);
    o.put(0, 1);
    o.put(1, 1); // non-key
    o.put(0, 1); // hidden
    o.put(err as u32, 1);
    o.put(1, 1); // intra_only
    if !err {
        o.put(reset, 2);
    }
    o.put(0x498342, 24);
    o.put(refresh as u32, 8);
    o.copy(frame, size_at, after_render);
    o.copy(frame, after_render, end);
    let mut out = o.bytes();
    out.extend_from_slice(&frame[end.div_ceil(8)..]);
    out
}

fn reference(ivf: &Path, pix_fmt: &str) -> Vec<u8> {
    let yuv = ivf.with_extension("yuv");
    let _ = std::fs::remove_file(&yuv);
    assert!(common::reference_decode(ivf, &yuv, pix_fmt), "ffmpeg failed on {}", ivf.display());
    let r = std::fs::read(&yuv).unwrap();
    let _ = std::fs::remove_file(&yuv);
    r
}

fn encode(name: &str, w: u32, h: u32, frames: u32, extra: &[&str]) -> Option<Vec<Vec<u8>>> {
    let ff = common::ffmpeg()?;
    let path = common::fixtures_dir().join(format!("{name}.ivf"));
    if !path.exists() {
        let mut c = std::process::Command::new(ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"]);
        c.arg(format!("testsrc2=size={w}x{h}:rate=25,noise=alls=8:allf=t+u"));
        c.args(["-frames:v", &frames.to_string(), "-c:v", "libvpx-vp9", "-b:v", "500k", "-frame-parallel", "0"]);
        c.args(extra);
        // written aside and renamed: a test in another process may be reading this fixture
        let tmp = path.with_extension(format!("tmp.{}.ivf", common::unique()));
        c.args(["-f", "ivf"]).arg(&tmp);
        if !common::run(&mut c) {
            return None;
        }
        if std::fs::rename(&tmp, &path).is_err() && !path.exists() {
            return None;
        }
    }
    let d = std::fs::read(&path).unwrap();
    Some(common::ivf_frames(&d).into_iter().map(|f| f.to_vec()).collect())
}

fn check(name: &str, frames: &[Vec<u8>]) {
    let path = common::fixtures_dir().join(format!("{name}.ivf"));
    common::write_ivf(&path, 0, 0, frames);
    let r = reference(&path, "yuv420p");
    common::check_files(name, &path, &r, "yuv420p").unwrap();
    let _ = std::fs::remove_file(path);
}

/// Splice the inter frames of a `w2` x `h2` stream after frames of a 352x288 stream.
fn scaled_splice(name: &str, w2: u32, h2: u32, interp: &[&str]) -> Option<Vec<Vec<u8>>> {
    let a = encode("syn_base_352", 352, 288, 6, &[])?;
    let mut args = vec!["-error-resilient", "1"];
    args.extend_from_slice(interp);
    let b = encode(&format!("syn_er_{w2}x{h2}{}", interp.join("")), w2, h2, 10, &args)?;
    let mut frames: Vec<Vec<u8>> = a[..4].to_vec();
    for f in &b[1..] {
        frames.push(explicit_size(f, w2, h2));
    }
    let _ = name;
    Some(frames)
}

#[test]
fn scaled_reference_downscale() {
    let Some(f) = scaled_splice("down", 256, 208, &[]) else { return };
    let mut dec = filmcraft_vp9::Decoder::with_threads(1);
    for c in &f {
        let _ = dec.decode(c, 0);
    }
    assert!(dec.stats().scaled_ref_blocks > 0, "{:?}", dec.stats());
    check("syn_scaled_down", &f);
}

#[test]
fn splice_same_size_control() {
    let Some(f) = scaled_splice("same", 352, 288, &[]) else { return };
    check("syn_same", &f);
}

#[test]
fn scaled_reference_upscale() {
    let Some(f) = scaled_splice("up", 480, 400, &[]) else { return };
    check("syn_scaled_up", &f);
}

#[test]
fn scaled_reference_odd_sizes() {
    let Some(f) = scaled_splice("odd", 301, 199, &[]) else { return };
    check("syn_scaled_odd", &f);
}

fn build_superframe(frames: &[Vec<u8>]) -> Vec<u8> {
    if frames.len() == 1 {
        return frames[0].clone();
    }
    let mut out = Vec::new();
    for f in frames {
        out.extend_from_slice(f);
    }
    let marker = 0xc0 | (3 << 3) | (frames.len() as u8 - 1);
    out.push(marker);
    for f in frames {
        out.extend((f.len() as u32).to_le_bytes());
    }
    out.push(marker);
    out
}

#[test]
fn intra_only_frames() {
    // Error resilient source: the frame after the intra-only frame does not use the previous
    // frame's motion vectors either way, so its symbols parse as the encoder intended.
    let Some(src) = encode("syn_er_g8", 352, 288, 20, &["-g", "8", "-error-resilient", "1"]) else { return };
    let keys: Vec<usize> = src.iter().enumerate().filter(|(_, f)| f[0] & 0x04 == 0 && f[0] & 0x08 == 0).map(|(i, _)| i).collect();
    assert!(keys.len() >= 2, "need a second key frame: {keys:?}");
    for (reset, refresh, show_slot) in [(3u32, 0xffu8, 0u8), (2, 0x05, 2), (0, 0x10, 4)] {
        let mut frames = Vec::new();
        for (i, f) in src.iter().enumerate() {
            if i == keys[1] {
                frames.push(key_to_intra_only(f, refresh, reset));
                frames.push(vec![0x88 | show_slot]);
            } else {
                frames.push(f.clone());
            }
        }
        let mut dec = filmcraft_vp9::Decoder::with_threads(1);
        for c in &frames {
            dec.decode(c, 0).unwrap();
        }
        let s = dec.stats();
        assert!(s.intra_only == 1 && s.show_existing == 1, "{s:?}");
        check(&format!("syn_intra_only_{reset}"), &frames);
    }
}

#[test]
fn show_existing_frames() {
    let Some(src) = encode("syn_er_352", 352, 288, 6, &["-error-resilient", "1"]) else { return };
    let mut frames = Vec::new();
    for (i, f) in src.iter().enumerate() {
        frames.push(f.clone());
        if i >= 1 {
            frames.push(vec![0x88 | (i as u8 % 8)]);
        }
    }
    // A superframe holding a frame and a show_existing_frame.
    let sf = build_superframe(&[src[2].clone(), vec![0x88]]);
    frames.push(sf);
    check("syn_show_existing", &frames);
}

//! Black-box probes of ffmpeg's VC-3 decoder (ignored tests, run by hand).
//!
//! Each probe writes a stream whose first DCT block holds one AC coefficient (all other blocks
//! empty), decodes it with ffmpeg as an external process, and measures the reconstructed
//! coefficient with a forward DCT of the output pixels. They established the two places where
//! ffmpeg's decoder differs from the text of SMPTE ST 2019-1 (see the README): the quantisation
//! weights it uses for CID 1271, and its reconstruction offset.
//!
//! `DNX_CID=1271 cargo test -p filmcraft-dnx --lib ffmpeg_probe -- --ignored --nocapture`
use crate::bits::Writer;
use crate::header::{self, EOF_SIGNATURE, FieldCode, FrameHeader, SCAN_INDEX_OFFSET};
use crate::tables::{ZIGZAG, cid_info, vlc};
use crate::{ChromaFormat, ColorVolume};

struct ProbeStream {
    cid: u32,
    width: u32,
    height: u32,
    depth: u8,
    quantizer: u32,
    coefficient_position: usize,
    amplitude: u32,
    negative: bool,
    block: usize,
}

fn stream_cid(input: ProbeStream) -> Vec<u8> {
    let ProbeStream { cid, width: w, height: h, depth, quantizer: q, coefficient_position: pos, amplitude: amp, negative: neg, block: blk } = input;
    let info = cid_info(cid).unwrap();
    let v = vlc(info.vlc);
    let p_bits = if depth == 8 { 4 } else { 6 };
    let nw = w.div_ceil(16) as usize;
    let ns = h.div_ceil(16) as usize;
    let mut rows = Vec::new();
    for s in 0..ns {
        let mut wr = Writer::new();
        for mb in 0..nw {
            wr.put(q, 11);
            wr.put(0, 1);
            for k in 0..8 {
                let (c, l) = v.dc_code[0];
                wr.put(c as u32, l as u32);
                if s == 0 && mb == 0 && k == blk {
                    let p = (amp - 1) >> 6;
                    let base = ((amp - 1) & 63) as usize;
                    let frun = (pos > 1) as usize;
                    let (c, l) = v.amp_code[frun][(p > 0) as usize][base];
                    wr.put(c as u32, l as u32);
                    wr.put(neg as u32, 1);
                    if p > 0 {
                        wr.put(p, p_bits);
                    }
                    if frun == 1 {
                        let (c, l) = v.run_code[pos - 1];
                        wr.put(c as u32, l as u32);
                    }
                }
                wr.put(v.eob.0 as u32, v.eob.1 as u32);
            }
        }
        rows.push(wr.finish_aligned(4));
    }
    let hdr = FrameHeader {
        header_size: 0x280,
        version: if cid >= 1270 { 3 } else { 1 },
        cid,
        vbr: false,
        field: FieldCode::Frame,
        macf: false,
        crc: false,
        alpha: false,
        lossless_alpha: false,
        premultiplied_alpha: false,
        width: w,
        lines: h,
        par: (0, 0),
        bit_depth: depth,
        interlaced: false,
        frame_encoding: true,
        chroma: ChromaFormat::Yuv422,
        rgb: false,
        color_volume: ColorVolume::Bt709,
        timecode: None,
        mb_rows: ns as u32,
    };
    let mut out = header::write(&hdr);
    let mut off = 0u32;
    for (s, r) in rows.iter().enumerate() {
        out[SCAN_INDEX_OFFSET + 4 * s..SCAN_INDEX_OFFSET + 4 * s + 4].copy_from_slice(&off.to_be_bytes());
        off += r.len() as u32;
    }
    for r in rows {
        out.extend_from_slice(&r);
    }
    let size = match info.kind {
        crate::tables::Kind::Hd { frame_size, .. } => frame_size as usize,
        crate::tables::Kind::Ri { c0 } => crate::ri_frame_size(w, h, c0, false) as usize,
    };
    out.resize(size - 4, 0);
    out.extend_from_slice(&EOF_SIGNATURE);
    out
}

#[test]
#[ignore]
fn probe_weights() {
    let ff = filmcraft_testkit::ffmpeg().unwrap();
    let dir = filmcraft_testkit::fixtures_dir("dnx");
    let cid: u32 = std::env::var("DNX_CID").unwrap().parse().unwrap();
    let (w, h, depth): (u32, u32, u8) = match cid {
        1250 => (1280, 720, 10),
        1251 | 1252 => (1280, 720, 8),
        1235 => (1920, 1080, 10),
        1271 => (256, 128, 10),
        1272..=1274 => (256, 128, 8),
        c => {
            let i = cid_info(c).unwrap();
            match i.kind {
                crate::tables::Kind::Hd { width, height, depth, .. } => (width as u32, height as u32, depth),
                _ => panic!(),
            }
        }
    };
    let pix = if depth == 10 { "yuv422p10le" } else { "yuv422p" };
    let prof = cid;
    let amp = 20u32;
    let q = 4u32;
    for blk in [0usize, 2] {
        let mut grid = [[0f64; 8]; 8];
        for pos in 1..64 {
            let s = stream_cid(ProbeStream {
                cid,
                width: w,
                height: h,
                depth,
                quantizer: q,
                coefficient_position: pos,
                amplitude: amp,
                negative: false,
                block: blk,
            });
            let path = dir.join("explore.dnxhd");
            std::fs::write(&path, &s).unwrap();
            let out = std::process::Command::new(&ff)
                .args(["-hide_banner", "-loglevel", "error", "-f", "dnxhd", "-i"])
                .arg(&path)
                .args(["-f", "rawvideo", "-pix_fmt", pix, "-"])
                .output()
                .unwrap();
            let px: Vec<f64> = if depth == 8 {
                out.stdout.iter().map(|&b| b as f64).collect()
            } else {
                out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]]) as f64).collect()
            };
            let (off, stride) = if blk == 0 { (0, w as usize) } else { ((w * h) as usize, w as usize / 2) };
            let mid = (1 << (depth - 1)) as f64;
            let z = ZIGZAG[pos] as usize;
            let (u, vv) = (z % 8, z / 8);
            let mut sum = 0f64;
            for j in 0..8 {
                for i in 0..8 {
                    let cu = if u == 0 { 0.5f64.sqrt() } else { 1.0 };
                    let cv = if vv == 0 { 0.5f64.sqrt() } else { 1.0 };
                    sum += cu * cv / 4.0
                        * (px[off + j * stride + i] - mid)
                        * (((2 * i + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()
                        * (((2 * j + 1) * vv) as f64 * std::f64::consts::PI / 16.0).cos();
                }
            }
            grid[vv][u] = sum * cid_info(cid).unwrap().p as f64 / ((amp as f64 + 0.5) * q as f64);
        }
        {
            use crate::spec_tables::*;
            let all: [(&str, &[[u8; 63]; 2]); 11] = [
                ("D1", &W_D_1),
                ("D2", &W_D_2),
                ("D3", &W_D_3),
                ("D4", &W_D_4),
                ("D5", &W_D_5),
                ("D6", &W_D_6),
                ("D7", &W_D_7),
                ("D8", &W_D_8),
                ("D9", &W_D_9),
                ("D10", &W_D_10),
                ("D11", &W_D_11),
            ];
            let mut res = vec![];
            for (n, t) in all {
                for c in 0..2 {
                    let mut e = 0f64;
                    for i in 1..64 {
                        e += (grid[i / 8][i % 8] - t[c][i - 1] as f64).powi(2);
                    }
                    res.push(((e / 63.0).sqrt(), format!("{n}{}", if c == 0 { "L" } else { "C" })));
                }
            }
            res.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            println!("best matches: {:?}", &res[..4]);
        }
        println!("{prof:?} block {blk} estimated W (spec table below):");
        let w = &cid_info(cid).unwrap().weights[(blk != 0) as usize];
        for v in 0..8 {
            let mut l = String::new();
            for u in 0..8 {
                l += &format!("{:6.1}", grid[v][u]);
            }
            l += "   |";
            for u in 0..8 {
                if v + u > 0 {
                    l += &format!("{:4}", w[v * 8 + u - 1]);
                } else {
                    l += "   -";
                }
            }
            println!("{l}");
        }
    }
}

#[test]
#[ignore]
fn probe_reconstruction() {
    let ff = filmcraft_testkit::ffmpeg().unwrap();
    let dir = filmcraft_testkit::fixtures_dir("dnx");
    let cid: u32 = std::env::var("DNX_CID").unwrap().parse().unwrap();
    let (w, h, depth): (u32, u32, u8) = match cid {
        1250 => (1280, 720, 10),
        1251 | 1252 => (1280, 720, 8),
        1235 => (1920, 1080, 10),
        1271 => (256, 128, 10),
        _ => (256, 128, 8),
    };
    let pix = if depth == 10 { "yuv422p10le" } else { "yuv422p" };
    let p = cid_info(cid).unwrap().p as f64;
    for q in [1u32, 2, 3, 4, 8, 16] {
        let mut xs = vec![];
        for amp in 1..=40u32 {
            let s =
                stream_cid(ProbeStream { cid, width: w, height: h, depth, quantizer: q, coefficient_position: 1, amplitude: amp, negative: false, block: 0 });
            let path = dir.join("explore.dnxhd");
            std::fs::write(&path, &s).unwrap();
            let out = std::process::Command::new(&ff)
                .args(["-hide_banner", "-loglevel", "error", "-f", "dnxhd", "-i"])
                .arg(&path)
                .args(["-f", "rawvideo", "-pix_fmt", pix, "-"])
                .output()
                .unwrap();
            let px: Vec<f64> = if depth == 8 {
                out.stdout.iter().map(|&b| b as f64).collect()
            } else {
                out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]]) as f64).collect()
            };
            let mid = (1 << (depth - 1)) as f64;
            let mut sum = 0f64;
            for j in 0..8 {
                for i in 0..8 {
                    sum += 0.25 * (px[j * w as usize + i] - mid) * ((2 * i + 1) as f64 * std::f64::consts::PI / 16.0).cos() * std::f64::consts::FRAC_1_SQRT_2;
                }
            }
            xs.push((amp as f64, sum));
        }
        let n = xs.len() as f64;
        let sx: f64 = xs.iter().map(|x| x.0).sum();
        let sy: f64 = xs.iter().map(|x| x.1).sum();
        let sxx: f64 = xs.iter().map(|x| x.0 * x.0).sum();
        let sxy: f64 = xs.iter().map(|x| x.0 * x.1).sum();
        let a = (n * sxy - sx * sy) / (n * sxx - sx * sx);
        let b = (sy - a * sx) / n;
        let step = 32.0 * q as f64 / p;
        println!("cid {cid} q {q}: step {step} slope {a:.4} ({:.4} steps) intercept {b:.3} ({:.4} steps)", a / step, b / step);
    }
}

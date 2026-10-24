//! Decode an Annex-B H.265 file to raw planar YUV (yuv420p or yuv420p10le):
//! `cargo run --release -p filmcraft-hevc --example hevcdec -- in.hevc out.yuv`

use filmcraft_hevc::Decoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: hevcdec <in.hevc> [out.yuv]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).expect("read input");
    let t0 = std::time::Instant::now();
    // HEVC_THREADS=n sets the thread count, HEVC_LOOPS=k decodes the file k times (benchmarking),
    // HEVC_AU=1 feeds one access unit per `decode` call and drops the pictures (like a player or
    // `ffmpeg -f null`; no output file then).
    let threads = std::env::var("HEVC_THREADS").ok().and_then(|s| s.parse().ok()).unwrap_or(0usize);
    let loops = std::env::var("HEVC_LOOPS").ok().and_then(|s| s.parse().ok()).unwrap_or(1usize);
    let per_au = std::env::var_os("HEVC_AU").is_some();
    let aus = if per_au { access_units(&data) } else { Vec::new() };
    let mut pics = Vec::new();
    let mut count = 0;
    for _ in 0..loops {
        let mut dec = if threads > 0 { Decoder::with_threads(threads) } else { Decoder::new() };
        // HEVC_DRAFT=1: draft mode (non-reference pictures skip deblocking and SAO).
        dec.set_draft(std::env::var_os("HEVC_DRAFT").is_some());
        // HEVC_INTRA=0 / 1: decoding inside pictures on several threads off / on (default: auto).
        dec.set_intra_picture_parallel(std::env::var("HEVC_INTRA").ok().map(|v| v != "0"));
        if per_au {
            count = 0;
            for (i, au) in aus.iter().enumerate() {
                count += dec.decode(au, i as i64).expect("decode").len();
            }
            count += dec.flush().len();
            continue;
        }
        pics = dec.decode(&data, 0).expect("decode");
        pics.extend(dec.flush());
        count = pics.len();
    }
    let secs = t0.elapsed().as_secs_f64();
    eprintln!("{} pictures in {:.3}s ({:.1} fps)", count, secs, count as f64 / secs);
    if let Some(out) = args.get(2) {
        let mut buf = Vec::new();
        for p in &pics {
            buf.extend(p.y.to_le_bytes());
            buf.extend(p.u.to_le_bytes());
            buf.extend(p.v.to_le_bytes());
        }
        std::fs::write(out, buf).expect("write output");
    }
}

/// Split an Annex-B stream into access units: one starts at a VPS / SPS / PPS / AUD / prefix SEI
/// after a slice, or at a slice with first_slice_segment_in_pic_flag set.
fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut aus = Vec::new();
    let (mut start, mut seen_slice) = (0, false);
    let mut i = 0;
    while i + 5 < data.len() {
        if data[i] != 0 || data[i + 1] != 0 || data[i + 2] != 1 {
            i += 1;
            continue;
        }
        let sc = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
        let t = (data[i + 3] >> 1) & 0x3f;
        let slice = t < 32;
        if seen_slice && (matches!(t, 32..=35 | 39) || (slice && data[i + 5] & 0x80 != 0)) {
            aus.push(&data[start..sc]);
            start = sc;
            seen_slice = false;
        }
        seen_slice |= slice;
        i += 3;
    }
    aus.push(&data[start..]);
    aus
}

//! Decode an Annex-B H.265 file to raw planar YUV (yuv420p or yuv420p10le):
//! `cargo run --release -p filmcraft-hevc --example hevcdec -- in.hevc out.yuv`

use filmcraft_hevc::Decoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: hevcdec <in.hevc> [out.yuv]");
        std::process::exit(2);
    }
    if args.len() > 2 && std::env::var_os("HEVC_AU").is_some() {
        eprintln!("HEVC_AU drops the decoded pictures, so it cannot write an output file; remove {} or HEVC_AU", args[2]);
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
            count += flush_checked(&mut dec).len();
            continue;
        }
        pics = dec.decode(&data, 0).expect("decode");
        pics.extend(flush_checked(&mut dec));
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

/// Flush the decoder and fail on an error from work that was still in flight (a picture that
/// failed after `decode` returned would otherwise count as decoded).
fn flush_checked(dec: &mut Decoder) -> Vec<filmcraft_hevc::Picture> {
    let pics = dec.flush();
    if let Some(e) = dec.take_error() {
        eprintln!("decode: {e}");
        std::process::exit(1);
    }
    pics
}

/// Split an Annex-B stream into access units. A new one starts at a slice with
/// first_slice_segment_in_pic_flag set, or at the VPS / SPS / PPS / AUD / prefix SEI that precedes
/// it. A parameter set or prefix SEI between two slice segments of one picture does not end it, so
/// the boundary is decided only when the next slice NAL unit shows whether it starts a picture.
fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut aus = Vec::new();
    let (mut start, mut seen_slice) = (0, false);
    // first VPS / SPS / PPS / AUD / prefix SEI after the last slice, not yet known to be a boundary
    let mut pending: Option<usize> = None;
    let mut i = 0;
    while i + 5 < data.len() {
        if data[i] != 0 || data[i + 1] != 0 || data[i + 2] != 1 {
            i += 1;
            continue;
        }
        let sc = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
        let t = (data[i + 3] >> 1) & 0x3f;
        if t < 32 {
            if seen_slice && data[i + 5] & 0x80 != 0 {
                let b = pending.unwrap_or(sc);
                aus.push(&data[start..b]);
                start = b;
            }
            seen_slice = true;
            pending = None;
        } else if seen_slice && pending.is_none() && matches!(t, 32..=35 | 39) {
            pending = Some(sc);
        }
        i += 3;
    }
    aus.push(&data[start..]);
    aus
}

#[cfg(test)]
mod tests {
    use super::access_units;

    /// A NAL unit with a 4-byte start code; slices get first_slice_segment_in_pic_flag in the payload.
    fn nal(t: u8, first: bool) -> Vec<u8> {
        vec![0, 0, 0, 1, t << 1, 1, if first { 0x80 } else { 0 }, 0x11]
    }

    #[test]
    fn a_prefix_sei_between_slice_segments_stays_in_the_picture() {
        let stream: Vec<u8> = [nal(32, false), nal(1, true), nal(39, false), nal(1, false), nal(39, false), nal(1, true), nal(1, true)].concat();
        let aus = access_units(&stream);
        assert_eq!(aus.len(), 3);
        assert_eq!(aus.concat(), stream);
        // VPS + first slice + SEI + continuation | SEI + new picture | new picture
        assert_eq!(aus[0].len(), 4 * 8);
        assert_eq!(aus[1].len(), 2 * 8);
        assert_eq!(aus[2].len(), 8);
    }

    #[test]
    fn parameter_sets_after_a_picture_start_the_next_one() {
        let stream: Vec<u8> = [nal(1, true), nal(32, false), nal(33, false), nal(34, false), nal(19, true)].concat();
        let aus = access_units(&stream);
        assert_eq!(aus.iter().map(|a| a.len()).collect::<Vec<_>>(), vec![8, 4 * 8]);
    }
}

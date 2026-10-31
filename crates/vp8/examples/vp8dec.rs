//! Decode an IVF file to raw I420 (`vp8dec in.ivf out.yuv`), for comparison with other decoders.
//! With `VP8DEC_SIZES` set, prints the size of each picture; `VP8_THREADS` sets the thread count
//! (default: all cores) and `VP8_LOOPS` decodes the file that many times (for timing).

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output] = args.as_slice() else {
        return Err("usage: vp8dec in.ivf out.yuv".into());
    };
    let data = std::fs::read(input)?;
    let hdr_len = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    let threads = std::env::var("VP8_THREADS").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut dec = if threads > 0 { filmcraft_vp8::Decoder::with_threads(threads) } else { filmcraft_vp8::Decoder::new() };
    let loops = std::env::var("VP8_LOOPS").ok().and_then(|s| s.parse().ok()).unwrap_or(1usize);
    let mut n = 0;
    let t = std::time::Instant::now();
    for _ in 0..loops {
        let mut pos = hdr_len;
        while pos + 12 <= data.len() {
            let size = u32::from_le_bytes(data[pos..pos + 4].try_into()?) as usize;
            let pts = i64::from_le_bytes(data[pos + 4..pos + 12].try_into()?);
            pos += 12;
            let frame = &data[pos..(pos + size).min(data.len())];
            pos += size;
            match dec.decode(frame, pts) {
                Ok(pictures) => n += write(&mut out, &pictures)?,
                Err(e) => eprintln!("frame at pts {pts}: {e}"),
            }
        }
    }
    n += write(&mut out, &dec.flush())?;
    let secs = t.elapsed().as_secs_f64();
    eprintln!("{n} pictures in {secs:.3}s ({:.1} fps); {:?}", n as f64 / secs, dec.stats());
    Ok(())
}

/// The pictures as I420; how many.
fn write(out: &mut impl Write, pictures: &[filmcraft_vp8::Picture]) -> std::io::Result<usize> {
    for p in pictures {
        if std::env::var_os("VP8DEC_SIZES").is_some() {
            println!("{} {}", p.width, p.height);
        }
        out.write_all(&p.y)?;
        out.write_all(&p.u)?;
        out.write_all(&p.v)?;
    }
    Ok(pictures.len())
}

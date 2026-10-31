//! Decode an IVF file to raw I420 (`vp8dec in.ivf out.yuv`), for comparison with other decoders.
//! With `VP8DEC_SIZES` set, prints the size of each picture.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output] = args.as_slice() else {
        return Err("usage: vp8dec in.ivf out.yuv".into());
    };
    let data = std::fs::read(input)?;
    let hdr_len = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut pos = hdr_len;
    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    let mut dec = filmcraft_vp8::Decoder::new();
    let mut n = 0;
    let t = std::time::Instant::now();
    while pos + 12 <= data.len() {
        let size = u32::from_le_bytes(data[pos..pos + 4].try_into()?) as usize;
        let pts = i64::from_le_bytes(data[pos + 4..pos + 12].try_into()?);
        pos += 12;
        let frame = &data[pos..(pos + size).min(data.len())];
        pos += size;
        match dec.decode(frame, pts) {
            Ok(Some(p)) => {
                if std::env::var_os("VP8DEC_SIZES").is_some() {
                    println!("{} {}", p.width, p.height);
                }
                out.write_all(&p.y)?;
                out.write_all(&p.u)?;
                out.write_all(&p.v)?;
                n += 1;
            }
            Ok(None) => {}
            Err(e) => eprintln!("frame at pts {pts}: {e}"),
        }
    }
    eprintln!("{n} pictures in {:.2}s; {:?}", t.elapsed().as_secs_f64(), dec.stats());
    Ok(())
}

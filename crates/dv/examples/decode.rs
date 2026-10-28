use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut input = BufReader::new(File::open(args.next().ok_or("input required")?)?);
    let mut out = BufWriter::new(File::create_new(args.next().ok_or("output required")?)?);
    let mut data = vec![0; 120000];
    loop {
        if input.read(&mut data[..1])? == 0 {
            break;
        }
        input.read_exact(&mut data[1..])?;
        let f = filmcraft_dv::decode_frame(&data)?;
        out.write_all(&f.y)?;
        out.write_all(&f.cb)?;
        out.write_all(&f.cr)?;
    }
    out.flush()?;
    Ok(())
}

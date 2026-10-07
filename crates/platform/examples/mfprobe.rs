//! Diagnostics for the Windows backend: why does Media Foundation take or decline a stream, and
//! where does a decode spend its time? `mfprobe [--time] <file.mp4>...` (`--time` decodes every
//! sample and prints the GPU decode, GPU to CPU readback and plane conversion costs per picture).
#[cfg(target_os = "windows")]
fn main() {
    use filmcraft_codecs::VideoDecoder;
    println!("register: {:?}", filmcraft_platform::register());
    let time = std::env::args().any(|a| a == "--time");
    for path in std::env::args().skip(1).filter(|a| !a.starts_with("--")) {
        let Ok(bytes) = std::fs::read(&path) else {
            println!("{path}: unreadable");
            continue;
        };
        let Ok(file) = filmcraft_isobmff::open(bytes.clone()) else {
            println!("{path}: not an MP4");
            continue;
        };
        let Some(t) = file.track_of_kind(filmcraft_isobmff::TrackKind::Video) else {
            println!("{path}: no video");
            continue;
        };
        let Some(entry) = file.tracks[t].entries.first() else { continue };
        let Some(Ok(info)) = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry) else {
            println!("{path}: not avcC/hvcC");
            continue;
        };
        let mut d = match filmcraft_platform::media_foundation::MfDecoder::new(info) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}: declined: {e}");
                continue;
            }
        };
        println!("{path}: hardware ok ({} via {})", d.name(), d.mft_name());
        if !time {
            continue;
        }
        let (mut n, t0) = (0usize, std::time::Instant::now());
        for i in 0..file.tracks[t].samples.len() {
            let Ok(s) = file.read_sample(&bytes, t, i) else { break };
            match d.decode(&s, file.tracks[t].samples[i].pts) {
                Ok(f) => n += f.len(),
                Err(e) => {
                    println!("  error at sample {i}: {e}");
                    break;
                }
            }
        }
        n += d.flush().len();
        let wall = t0.elapsed();
        let t = d.timings();
        let per = |x: std::time::Duration| x.as_secs_f64() * 1000.0 / t.frames.max(1) as f64;
        println!(
            "  {n} pictures in {:.0} ms ({:.1} fps): DXVA decode {:.2} ms/picture, GPU to CPU readback {:.2} ms, plane conversion {:.2} ms",
            wall.as_secs_f64() * 1000.0,
            n as f64 / wall.as_secs_f64(),
            per(t.decode),
            per(t.readback),
            per(t.convert)
        );
    }
}
#[cfg(not(target_os = "windows"))]
fn main() {}

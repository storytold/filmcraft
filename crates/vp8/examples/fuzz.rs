//! Decode corrupted variants of IVF files and report panics (`fuzz iterations seed files...`).
//! Run in a debug build so arithmetic overflow panics too.

fn frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else { return out };
    let mut pos = usize::from(u16::from_le_bytes([h[0], h[1]]));
    while pos + 12 <= data.len() {
        let size = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        out.push(data[pos..(pos + size).min(data.len())].to_vec());
        pos += size;
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let iterations: usize = args[1].parse().unwrap_or(100);
    let mut seed: u64 = args[2].parse().unwrap_or(1);
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    std::panic::set_hook(Box::new(|_| {}));
    let mut panics = 0;
    let mut decoded = 0u64;
    for path in &args[3..] {
        let original = frames(&std::fs::read(path).unwrap_or_default());
        for it in 0..iterations {
            let mut fr = original.clone();
            let mode = rnd() % 6;
            for f in fr.iter_mut() {
                if f.is_empty() {
                    continue;
                }
                match mode {
                    0 => {
                        // a few bit flips per frame
                        for _ in 0..1 + rnd() % 4 {
                            let i = (rnd() as usize) % f.len();
                            f[i] ^= 1 << (rnd() % 8);
                        }
                    }
                    1 => {
                        let n = (rnd() as usize) % (f.len() + 1);
                        f.truncate(n);
                    }
                    2 => {
                        // random bytes in the header region (frame tag, sizes, first partition)
                        for _ in 0..1 + rnd() % 3 {
                            let i = (rnd() as usize) % f.len().min(24);
                            f[i] = rnd() as u8;
                        }
                    }
                    3 => {
                        for b in f.iter_mut().skip(10) {
                            if rnd() % 50 == 0 {
                                *b = rnd() as u8;
                            }
                        }
                    }
                    // trailing garbage on some frames
                    4 if rnd() % 3 == 0 => f.extend((0..rnd() % 64).map(|_| rnd() as u8)),
                    _ => {}
                }
            }
            if mode == 5 {
                // decode order shuffled: inter frames after the wrong references
                for i in (1..fr.len()).rev() {
                    let j = (rnd() as usize) % (i + 1);
                    fr.swap(i, j);
                }
            }
            let r = std::panic::catch_unwind(|| {
                let mut dec = filmcraft_vp8::Decoder::new();
                let mut n = 0u64;
                for (k, f) in fr.iter().enumerate() {
                    let _ = dec.is_disposable(f);
                    if dec.decode(f, k as i64).is_ok() {
                        n += 1;
                    }
                    let _ = filmcraft_vp8::keyframe_info(f);
                }
                n
            });
            match r {
                Ok(n) => decoded += n,
                Err(e) => {
                    panics += 1;
                    let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                    eprintln!("PANIC {path} iteration {it} mode {mode}: {msg}");
                }
            }
        }
    }
    eprintln!("done: {panics} panics, {decoded} frames decoded");
}

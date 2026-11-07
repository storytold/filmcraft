//! Decode speed (ignored; run with `cargo test --release -p filmcraft-mpeg4v --test perf --
//! --ignored --nocapture`): one thread, 720x404 at 1.5 Mb/s, the shape of a TV episode in Xvid.

mod common;

use common::*;

fn run(name: &str, spec: &Spec, ff: &std::path::Path) {
    let f = make(ff, spec).expect("fixture");
    let es = std::fs::read(&f).unwrap();
    let cs = chunks(&es);
    let mut best = f64::MAX;
    let mut frames = 0;
    for _ in 0..3 {
        let mut d = filmcraft_mpeg4v::Decoder::new();
        let t0 = std::time::Instant::now();
        let mut n = 0;
        for (i, c) in cs.iter().enumerate() {
            n += d.decode(c, i as i64).unwrap().len();
        }
        n += d.flush().len();
        best = best.min(t0.elapsed().as_secs_f64());
        frames = n;
    }
    let kbps = es.len() as f64 * 8.0 / (frames as f64 / 23.976) / 1000.0;
    println!("{name}: {frames} frames, {kbps:.0} kb/s, in {:.2} s = {:.0} fps ({:.2} ms a frame)", best, frames as f64 / best, best * 1e3 / frames as f64);
}

#[test]
#[ignore]
fn perf_sd_decode_fps() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    const SRC: &str = "testsrc2=size=720x404:rate=24000/1001,noise=alls=8:allf=t";
    let enc: &[&str] = &["-vtag", "XVID", "-b:v", "1500k", "-maxrate", "2500k", "-bufsize", "2M", "-g", "250", "-flags", "+mv4", "-mbd", "2"];
    run("720x404 Simple Profile", &Spec { name: "perf_sp", lavfi: SRC, frames: 2000, enc }, &ff);
    // every macroblock moving, at half-sample positions most of the time
    let moving = "testsrc2=size=720x404:rate=24000/1001,scroll=h=0.0021:v=0.0037,noise=alls=8:allf=t";
    let enc: &[&str] = &["-vtag", "XVID", "-b:v", "1500k", "-g", "250", "-flags", "+mv4", "-mbd", "2"];
    run("720x404 Simple Profile, all moving", &Spec { name: "perf_moving", lavfi: moving, frames: 2000, enc }, &ff);
    let enc: &[&str] = &["-vtag", "XVID", "-b:v", "1500k", "-g", "250", "-bf", "2", "-mbd", "2"];
    run("720x404 B-VOPs", &Spec { name: "perf_b2", lavfi: SRC, frames: 2000, enc }, &ff);
    let enc: &[&str] = &["-vtag", "XVID", "-b:v", "1500k", "-g", "250", "-bf", "2", "-flags", "+qpel", "-mbd", "2"];
    run("720x404 B-VOPs, quarter sample", &Spec { name: "perf_qpel", lavfi: SRC, frames: 2000, enc }, &ff);
}

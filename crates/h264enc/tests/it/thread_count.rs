//! The worker-thread count only changes scheduling: with the slice count fixed, 1, 2 and several threads
//! must produce byte-identical streams (also across the reuse of the per-picture buffers over many frames).

use crate::common::synth;
use filmcraft_h264enc::*;

fn encode(rate: RateControl, bframes: u8, threads: usize) -> Vec<u8> {
    let mut c = EncoderConfig::new(320, 192, 24, 1);
    c.rate = rate;
    c.bframes = bframes;
    c.slices = 3;
    c.threads = threads;
    let mut enc = Encoder::new(c).unwrap();
    let mut stream = Vec::new();
    for t in 0..24 {
        let f = synth(320, 192, t);
        for p in enc.encode(&f.frame(), t as i64).unwrap() {
            stream.extend(p.data);
        }
    }
    for p in enc.flush() {
        stream.extend(p.data);
    }
    stream
}

#[test]
fn stream_does_not_depend_on_thread_count() {
    let cases = [
        ("crf, 2 b-frames", RateControl::Crf(23.0), 2),
        ("vbr, 2 b-frames", RateControl::Vbr { target_kbps: 800, max_kbps: 1200 }, 2),
        ("qp, no b-frames", RateControl::Qp(28), 0),
    ];
    for (name, rate, bframes) in cases {
        let one = encode(rate, bframes, 1);
        assert!(!one.is_empty(), "{name}");
        for threads in [2, 4, 0] {
            assert!(one == encode(rate, bframes, threads), "{name}: {threads} threads differ from 1 thread");
        }
    }
}

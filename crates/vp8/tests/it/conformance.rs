//! Bit-exactness against libvpx (through ffmpeg) on generated fixtures covering every coding
//! tool: skipped when ffmpeg or its libvpx encoder is absent.

use crate::common;

fn check(name: &str) {
    let f = common::fixture(name);
    let Some((ivf, yuv)) = common::ensure(f) else { return };
    let data = std::fs::read(&ivf).unwrap();
    let reference = std::fs::read(&yuv).unwrap();
    let pictures = common::decode_all(&common::ivf_frames(&data));
    let (w, h) = (f.width as usize, f.height as usize);
    let size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    assert_eq!(pictures.len(), reference.len() / size, "{name}: picture count");
    for (i, p) in pictures.iter().enumerate() {
        assert_eq!((p.width as usize, p.height as usize), (w, h), "{name}: size of picture {i}");
        assert!(common::i420(p) == reference[i * size..(i + 1) * size], "{name}: picture {i} differs from libvpx");
    }
}

macro_rules! fixture_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check(stringify!($name));
            }
        )*
    };
}

fixture_tests!(
    default,
    odd_size,
    tiny,
    intra_only,
    noise_high_rate,
    near_lossless,
    coarse,
    partitions8,
    altref_2pass,
    sharpness7,
    version1_bilinear_simple,
    version2,
    version3_fullpixel,
    realtime_segments,
    screen,
    temporal_layers,
    hd_1080p,
);

/// Every thread count decodes to the same pictures: with more than one, the rows of each frame
/// go through a pipeline of threads (and with several token partitions, a wavefront). The last
/// stream changes size at each of its key frames.
#[test]
fn same_pictures_at_every_thread_count() {
    let names = ["default", "odd_size", "tiny", "intra_only", "partitions8", "altref_2pass", "version1_bilinear_simple", "realtime_segments", "hd_1080p"];
    let mut streams = Vec::new();
    for name in names {
        let Some((ivf, _)) = common::ensure(common::fixture(name)) else { return };
        streams.push((name, std::fs::read(&ivf).unwrap()));
    }
    let mut cases: Vec<(&str, Vec<&[u8]>)> = streams.iter().map(|(name, data)| (*name, common::ivf_frames(data))).collect();
    let resized = ["odd_size", "hd_1080p", "tiny", "partitions8"]
        .iter()
        .flat_map(|n| cases.iter().find(|(name, _)| name == n).map(|c| c.1.clone()).unwrap_or_default())
        .collect();
    cases.push(("odd_size, hd_1080p, tiny, partitions8", resized));
    for (name, frames) in cases {
        let decode = |threads: usize| {
            let mut dec = filmcraft_vp8::Decoder::with_threads(threads);
            let mut pictures: Vec<_> = frames.iter().enumerate().flat_map(|(i, f)| dec.decode(f, i as i64).unwrap()).collect();
            pictures.extend(dec.flush());
            pictures.iter().map(|p| (p.width, p.pts, common::i420(p))).collect::<Vec<_>>()
        };
        let one = decode(1);
        assert!(!one.is_empty(), "{name}: pictures");
        for threads in [2, 3, 4, 5, 8] {
            assert!(decode(threads) == one, "{name}: {threads} threads");
        }
    }
}

/// With frames in flight on the worker threads: a reset drops them (decoding resumes at the next
/// key frame, with the same pictures as a new decoder), flush returns every one still in flight,
/// and dropping the decoder returns.
#[test]
fn frames_in_flight() {
    let Some((ivf, _)) = common::ensure(common::fixture("hd_1080p")) else { return };
    let data = std::fs::read(&ivf).unwrap();
    let frames = common::ivf_frames(&data);
    let all = common::decode_all(&frames);
    for threads in [2, 4, 8] {
        let mut dec = filmcraft_vp8::Decoder::with_threads(threads);
        assert_eq!(dec.threads(), if cfg!(feature = "threads") { threads } else { 1 });
        let mut first: Vec<_> = frames.iter().enumerate().flat_map(|(i, f)| dec.decode(f, i as i64).unwrap()).collect();
        dec.reset();
        assert!(dec.flush().is_empty(), "{threads} threads: a reset drops the frames in flight");
        assert!(dec.decode(frames[1], 1).is_err(), "{threads} threads: an inter frame after a reset");
        let mut again: Vec<_> = frames.iter().enumerate().flat_map(|(i, f)| dec.decode(f, i as i64).unwrap()).collect();
        again.extend(dec.flush());
        assert!(again.iter().map(common::i420).eq(all.iter().map(common::i420)), "{threads} threads: the pictures after a reset");
        assert!(first.len() <= all.len());
        first.clear();
        // frames in flight when the decoder goes
        for (i, f) in frames.iter().enumerate() {
            let _ = dec.decode(f, i as i64);
        }
        drop(dec);
    }
}

/// Frame threads under random use: streams of different sizes (with hidden frames and several
/// token partitions) one after the other in random order, decode calls interleaved with
/// flushes, flush-and-reset and (without a flush first) plain resets and drops. With a flush
/// before each reset the pictures are those of one thread; without, decoding goes on and
/// nothing hangs (each round runs under a deadline).
#[test]
fn frame_threads_under_random_use() {
    let names = ["odd_size", "tiny", "partitions8", "hd_1080p", "altref_2pass", "realtime_segments"];
    let mut streams = Vec::new();
    for name in names {
        let Some((ivf, _)) = common::ensure(common::fixture(name)) else { return };
        streams.push(std::fs::read(&ivf).unwrap());
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..24 {
        // a run of whole streams, each starting at its key frame
        let order: Vec<usize> = (0..6).map(|_| (rnd() % streams.len() as u64) as usize).collect();
        let frames: Vec<Vec<u8>> = order.iter().flat_map(|&s| common::ivf_frames(&streams[s]).into_iter().map(<[u8]>::to_vec)).collect();
        // per frame: 0 decode, 1 decode then flush, 2 decode then flush and reset, 3 decode then reset
        let ops: Vec<u64> = frames
            .iter()
            .map(|_| match rnd() % 16 {
                0 => 1,
                1 => 2,
                2 => 3,
                _ => 0,
            })
            .collect();
        let threads = 2 + (rnd() % 7) as usize;
        let exact = round % 2 == 0;
        let run = move |threads: usize, frames: Vec<Vec<u8>>, ops: Vec<u64>| {
            let mut dec = filmcraft_vp8::Decoder::with_threads(threads);
            let mut out = Vec::new();
            for (i, (f, &op)) in frames.iter().zip(&ops).enumerate() {
                out.extend(dec.decode(f, i as i64).unwrap_or_default().iter().map(|p| (p.pts, common::i420(p))));
                match op {
                    1 => out.extend(dec.flush().iter().map(|p| (p.pts, common::i420(p)))),
                    2 => {
                        out.extend(dec.flush().iter().map(|p| (p.pts, common::i420(p))));
                        dec.reset();
                    }
                    3 if !exact => dec.reset(),
                    _ => {}
                }
            }
            if exact {
                out.extend(dec.flush().iter().map(|p| (p.pts, common::i420(p))));
            }
            // without the last flush, the frames still in flight go with the decoder
            drop(dec);
            out
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (f, o) = (frames.clone(), ops.clone());
        std::thread::spawn(move || {
            let _ = tx.send(run(threads, f, o));
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap_or_else(|_| panic!("round {round}: {threads} threads hang"));
        if exact {
            assert!(got == run(1, frames, ops), "round {round}: {threads} threads, streams {order:?}");
        } else {
            assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "round {round}: pictures in order");
        }
    }
}

/// Hidden (altref) frames produce no picture, and leaving out the frames the decoder calls
/// disposable changes none of the other pictures.
#[test]
fn hidden_and_disposable_frames() {
    for name in ["altref_2pass", "temporal_layers"] {
        let f = common::fixture(name);
        let Some((ivf, _)) = common::ensure(f) else { return };
        let data = std::fs::read(&ivf).unwrap();
        let frames = common::ivf_frames(&data);
        // every picture, and which frames could be left out (asked right before each is decoded)
        let mut dec = filmcraft_vp8::Decoder::new();
        let mut all = Vec::new();
        let mut disposable = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            disposable.push(dec.is_disposable(f));
            all.extend(dec.decode(f, i as i64).unwrap().iter().map(|p| (p.pts, common::i420(p))));
        }
        all.extend(dec.flush().iter().map(|p| (p.pts, common::i420(p))));
        let stats = dec.stats();
        assert_eq!(stats.frames as usize, frames.len());
        assert_eq!(all.len() + stats.hidden_frames as usize, frames.len(), "{name}: one picture per shown frame");
        if name == "altref_2pass" {
            assert!(stats.hidden_frames > 0, "{name}: has hidden altref frames");
        }
        if name == "temporal_layers" {
            assert!(disposable.iter().filter(|&&d| d).count() >= frames.len() / 3, "{name}: the enhancement layer is disposable: {disposable:?}");
        }
        let mut dec = filmcraft_vp8::Decoder::new();
        for (i, f) in frames.iter().enumerate() {
            if disposable[i] {
                continue;
            }
            for p in dec.decode(f, i as i64).unwrap() {
                let reference = all.iter().find(|(pts, _)| *pts == p.pts).expect("picture of a shown frame");
                assert!(common::i420(&p) == reference.1, "{name}: picture {} changed after leaving out disposable frames", p.pts);
            }
        }
        for p in dec.flush() {
            let reference = all.iter().find(|(pts, _)| *pts == p.pts).expect("picture of a shown frame");
            assert!(common::i420(&p) == reference.1, "{name}: picture {} changed after leaving out disposable frames", p.pts);
        }
    }
}

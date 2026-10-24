//! Audio mixed ahead (`play_ahead`): driven by a wall-clock device. Its own test binary, because one
//! test panics a mixer on purpose and the crash hook's tests read the process-wide last panic.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use filmcraft_ui_egui::play_ahead::{AudioStats, BLOCK_FRAMES, LEAD_S, spawn};

const SR: u32 = 48_000;
const PERIOD: usize = 512;

/// A mixer whose every sample is its own device frame number, so the output shows which frame
/// played where.
fn frame_numbers(delay: Duration, calls: Arc<AtomicU64>) -> impl FnMut(i64, &mut [f32], usize) + Send + 'static {
    move |cursor, buf, ch| {
        calls.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(delay);
        for (i, f) in buf.chunks_mut(ch).enumerate() {
            f.fill((cursor + i as i64) as f32);
        }
    }
}

/// Waits (a bounded time) until the mixing thread has put `n` blocks in the ring (the first one
/// is mixed there, after `spawn` returns).
fn wait_for_blocks(stats: &AudioStats, n: u64) {
    let t0 = Instant::now();
    while stats.blocks_mixed.load(Ordering::Acquire) < n {
        assert!(t0.elapsed() < Duration::from_secs(10), "{n} blocks were never mixed");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Plays `callbacks` device periods, one every period on the wall clock as a device does, and
/// returns the interleaved output.
fn play(fill: &mut Box<dyn FnMut(&mut [f32], usize) + Send>, ch: usize, callbacks: usize) -> Vec<f32> {
    let period = Duration::from_secs_f64(PERIOD as f64 / SR as f64);
    let t0 = Instant::now();
    let mut out = Vec::new();
    for k in 0..callbacks {
        let mut buf = vec![f32::NAN; PERIOD * ch];
        fill(&mut buf, ch);
        out.extend_from_slice(&buf);
        if let Some(wait) = (period * (k as u32 + 1)).checked_sub(t0.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    out
}

#[test]
fn plays_every_frame_in_order_from_the_start() {
    let stats = Arc::new(AudioStats::default());
    let mut fill = spawn(frame_numbers(Duration::ZERO, Arc::default()), 1000, SR, 2, stats.clone());
    // let the mixer fill its lead first, as a device's start-up gives it time to
    wait_for_blocks(&stats, (LEAD_S * SR as f64) as u64 / BLOCK_FRAMES as u64 - 1);
    let out = play(&mut fill, 2, 60);
    for (p, f) in out.chunks(2).enumerate() {
        assert_eq!(f, [(1000 + p) as f32; 2], "frame {p}");
    }
    assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);
    assert_eq!(stats.callbacks.load(Ordering::Relaxed), 60);
}

#[test]
fn a_mixer_slower_than_the_device_plays_silence_and_stays_in_time() {
    // each 1024-frame block takes 40 ms to mix, about twice its 21 ms of sound
    let stats = Arc::new(AudioStats::default());
    let mut fill = spawn(frame_numbers(Duration::from_millis(40), Arc::default()), 1, SR, 2, stats.clone());
    let out = play(&mut fill, 2, 120);
    assert!(stats.underruns.load(Ordering::Relaxed) > 0);
    assert!(stats.missing_frames.load(Ordering::Relaxed) > 0);
    let mut heard = 0;
    for (p, f) in out.chunks(2).enumerate() {
        assert!(f.iter().all(|x| x.is_finite()), "frame {p} left unwritten");
        if f[0] != 0.0 {
            // every frame that plays is the one due at that moment: no drift after an underrun
            assert_eq!(f[0], (1 + p) as f32, "frame {p}");
            heard += 1;
        }
    }
    assert!(heard > 0 || stats.underruns.load(Ordering::Relaxed) > 0, "a mixer that never keeps up plays silence, never a frame out of time");
}

#[test]
fn a_slow_first_block_plays_silence_then_sound_in_time() {
    // The first block is mixed on the mixing thread, not by the caller (the UI thread): opening
    // the clip under the playhead can start a slow decoder (~0.1 s for some formats). The
    // device plays silence meanwhile, and what plays after is the frame due, not a late one.
    let stats = Arc::new(AudioStats::default());
    let mut first = true;
    let mix = move |cursor: i64, buf: &mut [f32], ch: usize| {
        let delay = if std::mem::take(&mut first) { Duration::from_millis(60) } else { Duration::ZERO };
        frame_numbers(delay, Arc::default())(cursor, buf, ch);
    };
    let started = Instant::now();
    let mut fill = spawn(mix, 1, SR, 2, stats.clone());
    assert!(started.elapsed() < Duration::from_millis(30), "spawn returns at once; the caller never mixes");
    let out = play(&mut fill, 2, 60);
    let first_heard = out.chunks(2).position(|f| f[0] != 0.0).expect("sound plays once the first block is mixed");
    assert!(first_heard > 0, "silence while the first block mixes");
    for (p, f) in out.chunks(2).enumerate().skip(first_heard) {
        assert_eq!(f[0], (1 + p) as f32, "frame {p} plays when it is due");
    }
}

#[test]
fn a_block_that_was_all_late_is_not_counted_as_mixed() {
    // A first block mixed after the device had played more than a block of silence put nothing
    // in the ring, yet counted as mixed: a wait for the first block went on to silence.
    let stats = Arc::new(AudioStats::default());
    let calls = Arc::new(AtomicU64::new(0));
    let (go1, wait1) = std::sync::mpsc::channel::<()>();
    let (go2, wait2) = std::sync::mpsc::channel::<()>();
    let counted = calls.clone();
    let mix = move |cursor: i64, buf: &mut [f32], ch: usize| {
        match counted.fetch_add(1, Ordering::AcqRel) {
            0 => drop(wait1.recv()),
            1 => drop(wait2.recv()),
            _ => {}
        }
        frame_numbers(Duration::ZERO, Arc::default())(cursor, buf, ch);
    };
    let mut fill = spawn(mix, 1, SR, 2, stats.clone());
    let wait_calls = |n: u64| {
        let t0 = Instant::now();
        while calls.load(Ordering::Acquire) < n {
            assert!(t0.elapsed() < Duration::from_secs(10), "the mixer was never called {n} times");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    // the first block is being mixed while the device plays two blocks of silence
    wait_calls(1);
    let mut buf = vec![f32::NAN; 2 * BLOCK_FRAMES * 2];
    fill(&mut buf, 2);
    assert!(buf.iter().all(|x| *x == 0.0));
    go1.send(()).unwrap();
    // the mixer has pushed the late block and is mixing the next one
    wait_calls(2);
    assert_eq!(stats.blocks_mixed.load(Ordering::Acquire), 0, "nothing of the late block is in the ring");
    go2.send(()).unwrap();
    wait_for_blocks(&stats, 1);
    let mut buf = vec![f32::NAN; PERIOD * 2];
    fill(&mut buf, 2);
    assert!(buf.iter().all(|x| *x != 0.0), "a counted block has sound in the ring");
}

#[test]
fn a_mixer_that_panics_leaves_silence_not_a_crash() {
    let stats = Arc::new(AudioStats::default());
    let mut first = true;
    let mix = move |cursor: i64, buf: &mut [f32], ch: usize| {
        if !first {
            panic!("mixer bug");
        }
        first = false;
        frame_numbers(Duration::ZERO, Arc::default())(cursor, buf, ch);
    };
    let mut fill = spawn(mix, 1, SR, 2, stats.clone());
    // the first block is mixed on the mixing thread: wait for it, as a device's start-up does
    wait_for_blocks(&stats, 1);
    let out = play(&mut fill, 2, 20);
    assert!(out.iter().all(|x| x.is_finite()));
    assert_eq!(out.get(..2), Some(&[1.0, 1.0][..]), "the block mixed before the panic plays");
    assert!(stats.underruns.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_device_with_other_channels_plays_the_channels_both_have() {
    for dev_ch in [1, 6, 10, 16] {
        let stats = Arc::new(AudioStats::default());
        let mut fill = spawn(frame_numbers(Duration::ZERO, Arc::default()), 1, SR, 2, stats.clone());
        // let the mixer get ahead first (the 4 callbacks play 2 blocks), as a device would before
        // its first callback
        wait_for_blocks(&stats, 2);
        let out = play(&mut fill, dev_ch, 4);
        for (p, f) in out.chunks(dev_ch).enumerate() {
            let want: Vec<f32> = (0..dev_ch).map(|c| if c < 2 { (1 + p) as f32 } else { 0.0 }).collect();
            assert_eq!(f, &want[..], "{dev_ch} channels, frame {p}");
        }
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 0, "{dev_ch} channels");
    }
}

#[test]
fn stopping_the_stream_stops_the_mixer() {
    let calls = Arc::new(AtomicU64::new(0));
    let fill = spawn(frame_numbers(Duration::ZERO, calls.clone()), 1, SR, 2, Arc::default());
    std::thread::sleep(Duration::from_millis(50));
    drop(fill);
    std::thread::sleep(Duration::from_millis(20));
    let after_stop = calls.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::Relaxed), after_stop);
}

#[test]
fn the_ring_keeps_no_more_than_the_lead() {
    let calls = Arc::new(AtomicU64::new(0));
    let _fill = spawn(frame_numbers(Duration::ZERO, calls.clone()), 1, SR, 2, Arc::default());
    std::thread::sleep(Duration::from_millis(100));
    let lead_blocks = (SR as f64 * LEAD_S / BLOCK_FRAMES as f64).ceil() as u64;
    assert!(calls.load(Ordering::Relaxed) <= lead_blocks + 1, "mixed {} blocks", calls.load(Ordering::Relaxed));
}

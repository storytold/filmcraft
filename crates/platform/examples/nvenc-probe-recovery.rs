//! Diagnostics: does a *transient* HEVC probe failure clear itself? `nvenc-probe-recovery`.
//!
//! A probe that fails can be failing for a moment rather than for ever - the GPU's encoder can be
//! held by another session. A failure cached for the rest of the process hides H.265 for the rest
//! of the session, so the answer has to be asked again after a while.
//!
//! This reproduces exactly that with the real driver: it takes the GPU's encoder sessions until
//! the GPU refuses another, asks NVENC's HEVC probe while they are held (the answer is "no"), then
//! releases them, waits out the retry window, and asks again. The answer must be "yes" the second
//! time - with the answer cached for the life of the process it would still be "no".
//!
//! It needs an NVIDIA GPU with an NVIDIA driver. On this machine (RTX 3090, driver 610.47) the
//! GPU takes 12 encoder sessions and refuses the 13th with `NV_ENC_ERR_OUT_OF_MEMORY` (21), which
//! is the driver saying there is no encoder session left.

use std::time::{Duration, Instant};

use filmcraft_platform::nvenc::{Config, Nvenc, Profile};

fn hevc_config() -> Config {
    Config {
        width: 640,
        height: 360,
        fps: (30, 1),
        bitrate_kbps: 2000,
        max_bitrate_kbps: 3000,
        cbr: false,
        keyint: 30,
        profile: Profile::HevcMain,
        level: None,
        sar: None,
        bframes: false,
    }
}

fn main() {
    if !filmcraft_platform::nvenc::available() {
        println!("no NVIDIA driver here: NVENC's probe cannot fail this way, so there is nothing to show");
        return;
    }

    // Take the GPU's encoder sessions until it refuses another.
    let mut held: Vec<Nvenc> = Vec::new();
    loop {
        match Nvenc::new(&hevc_config()) {
            Ok(e) => held.push(e),
            Err(why) => {
                println!("the GPU refuses another encoder session after {}: {why}", held.len());
                break;
            }
        }
        if held.len() > 64 {
            println!("the GPU took {} sessions: it does not run out, so this cannot be shown here", held.len());
            return;
        }
    }
    if held.is_empty() {
        println!("the GPU took no sessions: there is nothing to hold");
        return;
    }

    // While they are held, NVENC's HEVC probe cannot get a session: the answer is "no".
    let t = Instant::now();
    let while_held = filmcraft_platform::nvenc::hevc_available();
    println!("NVENC's HEVC probe while the encoder is fully taken: {while_held} (took {:?})", t.elapsed());
    assert!(!while_held, "the probe cannot get a session while the GPU's encoder is taken");

    // Release them. The failure is remembered for a while, so the answer does not change at once.
    drop(held);
    println!("released the GPU's encoder sessions");

    // Wait out the retry window, then ask again. The answer must recover.
    let mut waited = Duration::ZERO;
    let mut again = false;
    while waited < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_secs(5));
        waited += Duration::from_secs(5);
        again = filmcraft_platform::nvenc::hevc_available();
        if again {
            break;
        }
    }
    println!("NVENC's HEVC probe {waited:?} after the failure: {again}");
    assert!(again, "the probe must be asked again once the GPU is free: a failure must not be permanent");
    println!("so the answer recovers: a transient failure is not cached for the rest of the process");
}

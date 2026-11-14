//! Headless playback benchmark; see `bench/playback.rs` (`cargo xtask bench-playback`).

#![allow(dead_code)]

#[path = "bench/fixtures.rs"]
mod fixtures;
#[path = "bench/playback.rs"]
mod playback;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hw = args.iter().position(|a| a == "--hw").and_then(|i| args.get(i + 1)).is_none_or(|v| v != "off");
    // as the desktop app and `cargo xtask bench`: OS hardware decoders in front of ours, unless --hw off
    filmcraft_platform::register();
    filmcraft_codecs::hw::set_hardware_decoding(hw);
    playback::cli_main();
}

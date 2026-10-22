//! FilmCraft desktop app binary. The app itself is the library (`lib.rs`, [`filmcraft::run`]);
//! Android's NativeActivity entry point is the `filmcraft-android` crate.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

fn main() -> eframe::Result {
    filmcraft::run()
}

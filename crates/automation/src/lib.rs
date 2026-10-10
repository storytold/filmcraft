//! # filmcraft-automation
//!
//! The FilmCraft MCP server (official `rmcp` SDK, stdio transport).
//!
//! * **Headless** mode drives an in-process [`filmcraft_engine::Session`]: every engine command,
//!   project/sequence inspection and frame rendering (PNG) — no window needed.
//! * **Bridge** mode forwards to a running desktop app over the JSON-lines control channel
//!   (`filmcraft --control <port>`), so agents can also inspect, screenshot, click, drag, scroll
//!   and type in the live UI — every menu, panel, timeline gesture and keystroke.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod bridge;
pub mod long_job;
pub mod server;
pub mod ui_map;

pub use bridge::BridgeClient;
pub use server::FilmcraftMcp;

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Engine(#[from] filmcraft_engine::EngineError),
    #[error("bridge: {0}")]
    Bridge(String),
    #[error("app: {0}")]
    App(String),
    #[error("{0}")]
    Other(String),
}

/// Encode RGBA8 as PNG, downscaled so the longest side is at most `max_side` (0 = no limit).
pub fn png_rgba(w: u32, h: u32, rgba: Vec<u8>, max_side: u32) -> Result<Vec<u8>, AutomationError> {
    let mut img = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| AutomationError::Other("bad image".into()))?;
    if max_side > 0 && w.max(h) > max_side {
        let s = max_side as f32 / w.max(h) as f32;
        img = image::imageops::resize(&img, ((w as f32 * s) as u32).max(1), ((h as f32 * s) as u32).max(1), image::imageops::FilterType::Triangle);
    }
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| AutomationError::Other(e.to_string()))?;
    Ok(out.into_inner())
}

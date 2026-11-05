mod download_pacing;
mod recovery_policy;

// The pure page-config parsing, included as the crate's own module (the real crate is wasm32-only).
#[allow(dead_code)]
#[path = "../../src/web_config.rs"]
mod web_config;
mod web_config_tests;

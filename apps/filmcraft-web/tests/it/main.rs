mod download_pacing;
mod recovery_policy;

#[allow(dead_code)]
#[path = "../../src/atomwalk.rs"]
mod atomwalk;
mod atomwalk_tests;
#[allow(dead_code)]
#[path = "../../src/focus.rs"]
mod focus;
mod focus_tests;
#[allow(dead_code)]
#[path = "../../src/prewarm_plan.rs"]
mod prewarm_plan;
mod prewarm_plan_tests;
#[allow(dead_code)]
#[path = "../../src/readahead.rs"]
mod readahead;
mod readahead_tests;

// The pure remote-storage modules, included as the crate's own modules (the real crate is wasm32-only).
#[allow(dead_code)]
#[path = "../../src/remote_config.rs"]
mod remote_config;
mod remote_config_tests;
#[allow(dead_code)]
#[path = "../../src/remote_dav.rs"]
mod remote_dav;
mod remote_dav_tests;
#[allow(dead_code)]
#[path = "../../src/remote_lookup.rs"]
mod remote_lookup;
mod remote_lookup_tests;
#[allow(dead_code)]
#[path = "../../src/remote_roots.rs"]
mod remote_roots;
#[allow(dead_code)]
#[path = "../../src/remote_write.rs"]
mod remote_write;
mod remote_write_tests;

//! Print FilmCraft's effect and transition catalog as the Effects panel lists it, as JSON: one
//! object per item with its `id`, display `name`, `kind` (`Video`, `Audio`, `VideoTransition`,
//! `AudioTransition`) and Effects-panel folder `path` (e.g. `["Video Effects", "Blur & Sharpen"]`).
//! Items with no Effects-panel folder (intrinsic Motion / Opacity / Volume…, graphic layers) and
//! the hidden obsolete transitions are left out, as in the panel (the same top-level folder filter
//! as `panels/effects.rs`). `cargo xtask parity --area effects|transitions` reads this to measure
//! coverage against a reference list.
//!
//! ```sh
//! cargo run -p filmcraft-project --example effect_catalog
//! ```

use std::process::ExitCode;

use filmcraft_project::vtransition::EFFECT_TOP_FOLDERS;
use serde_json::json;

fn main() -> ExitCode {
    let items: Vec<serde_json::Value> = filmcraft_project::effect_defs()
        .iter()
        .filter(|d| d.category.first().is_some_and(|top| EFFECT_TOP_FOLDERS.contains(top)))
        .map(|d| json!({"id": d.id, "name": d.name, "kind": format!("{:?}", d.kind), "path": d.category}))
        .collect();
    match serde_json::to_string_pretty(&items) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("effect_catalog: {e}");
            ExitCode::FAILURE
        }
    }
}

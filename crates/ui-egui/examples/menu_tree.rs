//! Print FilmCraft's menu bar as it ships (English, default preferences) as JSON: one object per
//! item with its `id`, `label` and menu `path`. `cargo xtask parity` reads this to measure menu
//! coverage against a reference list.
//!
//! ```sh
//! cargo run -p filmcraft-ui-egui --example menu_tree
//! ```

use std::process::ExitCode;

use serde_json::json;

fn main() -> ExitCode {
    let items: Vec<serde_json::Value> =
        filmcraft_ui_egui::menus::default_menu_items().into_iter().map(|it| json!({"id": it.id, "label": it.label, "path": it.path})).collect();
    match serde_json::to_string_pretty(&items) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("menu_tree: {e}");
            ExitCode::FAILURE
        }
    }
}

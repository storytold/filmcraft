//! Optional craft-fonts build input (see storytold/craft-fonts `docs/integration.md`).
//!
//! With `CRAFT_FONTS_DIR=<absolute path of a craft-fonts checkout>` (a relative path resolves from
//! `crates/text`) this embeds every font listed in its
//! `fonts/manifest.txt` as [`CRAFT_FONTS`](crate::fonts::CRAFT_FONTS); unset, `CRAFT_FONTS` is
//! empty and nothing else changes. A `CRAFT_FONTS_DIR` that is not a checkout is a warning, or an
//! error with `CRAFT_FONTS_REQUIRED=1` (release builds), which also makes an unset or empty
//! `CRAFT_FONTS_DIR` an error.
//!
//! Native builds embed every font. The web (wasm32) build embeds only the UI font (BIZ UDPGothic
//! Regular, ~4.5 MB) and Noto Sans Thai (~0.2 MB, the one script the browser's own fonts may lack
//! that the UI draws in file names) to stay under the web size budget (Cloudflare serves files up
//! to 25 MiB). [`CRAFT_WASM_FACES`](crate::fonts::CRAFT_WASM_FACES) lists those faces on every target.
//! Only the local directory is read: no network.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_REQUIRED");
    let required = std::env::var_os("CRAFT_FONTS_REQUIRED").is_some_and(|v| v == "1");
    let mut src = String::from("pub static CRAFT_FONTS: &[CraftFont] = &[\n");
    let mut wasm_faces = String::new();
    match std::env::var_os("CRAFT_FONTS_DIR").filter(|d| !d.is_empty()).map(PathBuf::from) {
        Some(dir) => match craft_fonts(&dir) {
            Ok((entries, faces)) => {
                src.push_str(&entries);
                wasm_faces = faces;
            }
            Err(e) if required => println!("cargo::error=CRAFT_FONTS_DIR={}: {e}", dir.display()),
            Err(e) => println!("cargo::warning=building without craft-fonts: CRAFT_FONTS_DIR={}: {e}", dir.display()),
        },
        None if required => {
            println!("cargo::error=CRAFT_FONTS_REQUIRED=1 but CRAFT_FONTS_DIR is unset or empty: point it at a craft-fonts checkout (absolute path)");
        }
        None => {}
    }
    src.push_str("];\n");
    src.push_str("/// The (family, style) faces of [`CRAFT_FONTS`] the web (wasm32) build embeds; every target lists them.\n");
    src.push_str(&format!("pub static CRAFT_WASM_FACES: &[(&str, &str)] = &[\n{wasm_faces}];\n"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_fonts.rs");
    if let Err(e) = std::fs::write(&out, src) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}

/// The faces the web build embeds: the UI font and the Thai face.
fn wasm_keeps(family: &str, style: &str) -> bool {
    style == "Regular" && matches!(family, "BIZ UDPGothic" | "Noto Sans Thai")
}

/// One `CraftFont { .. }` initialiser per manifest line, and the `(family, style)` pairs the web
/// build embeds.
fn craft_fonts(dir: &std::path::Path) -> Result<(String, String), String> {
    let manifest = dir.join("fonts/manifest.txt");
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    let mut out = String::new();
    let mut faces = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        let keep = wasm_keeps(family, style);
        if wasm && !keep {
            continue;
        }
        let path = dir.join(file).canonicalize().map_err(|e| format!("{file}: {e}"))?;
        println!("cargo::rerun-if-changed={}", path.display());
        let scripts: Vec<String> = scripts.split(',').map(|s| format!("{:?}", s.trim())).collect();
        let _ = writeln!(
            out,
            "    CraftFont {{ family: {family:?}, style: {style:?}, scripts: &[{}], bytes: include_bytes!({:?}) }},",
            scripts.join(", "),
            path.display().to_string(),
        );
        if keep {
            let _ = writeln!(faces, "    ({family:?}, {style:?}),");
        }
    }
    Ok((out, faces))
}

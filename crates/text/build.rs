//! Optional craft-fonts build input (see storytold/craft-fonts `docs/integration.md` and
//! craftrules `standards/fonts.md`).
//!
//! With `CRAFT_FONTS_DIR=<absolute path of a craft-fonts checkout>` (a relative path resolves from
//! `crates/text`) this embeds every font listed in its
//! `fonts/manifest.txt` as [`CRAFT_FONTS`](crate::fonts::CRAFT_FONTS); unset, `CRAFT_FONTS` is
//! empty and nothing else changes. A `CRAFT_FONTS_DIR` that is not a checkout is a warning, or an
//! error with `CRAFT_FONTS_REQUIRED=1` (release builds), which also makes an unset or empty
//! `CRAFT_FONTS_DIR` an error.
//!
//! Native builds embed every font. The web (wasm32) build embeds only the UI font (BIZ UDPGothic
//! Regular, ~4.5 MB) to stay under the web size budget (Cloudflare serves files up to 25 MiB).
//! Only the local directory is read: no network.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_REQUIRED");
    let required = std::env::var_os("CRAFT_FONTS_REQUIRED").is_some_and(|v| v == "1");
    let mut src = String::from("pub static CRAFT_FONTS: &[CraftFont] = &[\n");
    match std::env::var_os("CRAFT_FONTS_DIR").filter(|d| !d.is_empty()).map(PathBuf::from) {
        Some(dir) => match craft_fonts(&dir) {
            Ok(entries) => src.push_str(&entries),
            Err(e) if required => println!("cargo::error=CRAFT_FONTS_DIR={}: {e}", dir.display()),
            Err(e) => println!("cargo::warning=building without craft-fonts: CRAFT_FONTS_DIR={}: {e}", dir.display()),
        },
        None if required => {
            println!("cargo::error=CRAFT_FONTS_REQUIRED=1 but CRAFT_FONTS_DIR is unset or empty: point it at a craft-fonts checkout (absolute path)");
        }
        None => {}
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_fonts.rs");
    if let Err(e) = std::fs::write(&out, src) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}

/// One `CraftFont { .. }` initialiser per manifest line.
fn craft_fonts(dir: &std::path::Path) -> Result<String, String> {
    let manifest = dir.join("fonts/manifest.txt");
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    let mut out = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        if wasm && !(*family == "BIZ UDPGothic" && *style == "Regular") {
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
    }
    Ok(out)
}

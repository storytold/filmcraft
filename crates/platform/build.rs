use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(filmcraft_ngx_sdk)");
    println!("cargo:rerun-if-env-changed=FILMCRAFT_NGX_SDK");
    let Some(root) = env::var_os("FILMCRAFT_NGX_SDK") else { return };
    if env::var("TARGET").as_deref() != Ok("x86_64-pc-windows-msvc") {
        return;
    }
    let lib = PathBuf::from(root).join("lib/Windows_x86_64/x64");
    let static_crt = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default().split(',').any(|feature| feature == "crt-static");
    let name = if static_crt { "nvsdk_ngx_s" } else { "nvsdk_ngx_d" };
    println!("cargo:rerun-if-changed={}", lib.join(format!("{name}.lib")).display());
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static={name}");
    println!("cargo:rustc-cfg=filmcraft_ngx_sdk");
}

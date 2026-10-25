fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=RED_R3D_SDK_DIR");
    println!("cargo:rerun-if-changed=src/red/helper.cpp");
    #[cfg(feature = "red-sdk")]
    build_red()?;
    Ok(())
}

/// The helper is built for the target the SDK ships libraries for: Windows x64 (MSVC) and Linux
/// x64 (the system C++ compiler). Every other target fails with a clear message.
#[cfg(feature = "red-sdk")]
fn build_red() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf};
    let target = env::var("TARGET")?;
    let linux = target == "x86_64-unknown-linux-gnu";
    if target != "x86_64-pc-windows-msvc" && !linux {
        return Err("red-sdk currently supports Windows x64 (MSVC) and Linux x64 (GNU) only".into());
    }
    // MSVC does not accept canonicalize()'s Windows verbatim prefix in /I arguments.
    let sdk = std::path::absolute(PathBuf::from(env::var("RED_R3D_SDK_DIR").map_err(|_| "Set RED_R3D_SDK_DIR to an extracted RED R3D SDK 9.2.1 folder")?))?;
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR missing")?);
    let header = sdk.join("Include/R3DSDK.h");
    println!("cargo:rerun-if-changed={}", header.display());
    let exe = if linux { build_red_linux(&sdk, &out)? } else { build_red_windows(&sdk, &out)? };
    println!("cargo:rustc-env=FILMCRAFT_BUILT_RED_HELPER={}", exe.display());
    Ok(())
}

#[cfg(feature = "red-sdk")]
fn build_red_windows(sdk: &std::path::Path, out: &std::path::Path) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let exe = out.join("filmcraft-red-helper.exe");
    let library = sdk.join("Lib/win64/R3DSDK-2017MD.lib");
    println!("cargo:rerun-if-changed={}", library.display());
    let compiler = cc::Build::new().cpp(true).get_compiler();
    let result = compiler
        .to_command()
        .args(["/nologo", "/std:c++17", "/EHsc", "/MD", "/O2", "/utf-8"])
        .arg(format!("/I{}", sdk.join("Include").display()))
        .arg("src/red/helper.cpp")
        .arg(format!("/Fo{}", out.join("red-helper.obj").display()))
        .arg(format!("/Fe{}", exe.display()))
        .arg(library)
        .status()?;
    if !result.success() {
        return Err("RED helper compilation failed".into());
    }
    for name in ["REDR3D-x64.dll", "REDDecoder-x64.dll", "REDCuda-x64.dll", "REDOpenCL-x64.dll"] {
        let dll = sdk.join("Redistributable/win").join(name);
        println!("cargo:rerun-if-changed={}", dll.display());
        std::fs::copy(dll, out.join(name))?;
    }
    Ok(exe)
}

/// Linux: the position-independent C++11-ABI static library (the system g++ defaults to PIE
/// executables and the new libstdc++ ABI), plus what the SDK's notes ask for: pthreads, dl and
/// libuuid. The runtime `.so` files land beside the helper; the Rust side points
/// `LD_LIBRARY_PATH` at that directory so `REDR3D-x64.so` finds `REDCuda-x64.so` and the others.
#[cfg(feature = "red-sdk")]
fn build_red_linux(sdk: &std::path::Path, out: &std::path::Path) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let exe = out.join("filmcraft-red-helper");
    let library = sdk.join("Lib/linux64/libR3DSDKPIC-cpp11.a");
    println!("cargo:rerun-if-changed={}", library.display());
    let compiler = cc::Build::new().cpp(true).get_compiler();
    let result = compiler
        .to_command()
        .args(["-std=c++17", "-O2", "-fPIC", "-pthread", "-Wall"])
        .arg(format!("-I{}", sdk.join("Include").display()))
        .arg("src/red/helper.cpp")
        .arg("-o")
        .arg(&exe)
        .arg(library)
        .args(["-ldl", "-luuid", "-pthread"])
        .status()?;
    if !result.success() {
        return Err("RED helper compilation failed".into());
    }
    for name in ["REDR3D-x64.so", "REDDecoder-x64.so", "REDCuda-x64.so", "REDOpenCL-x64.so"] {
        let so = sdk.join("Redistributable/linux").join(name);
        println!("cargo:rerun-if-changed={}", so.display());
        std::fs::copy(so, out.join(name))?;
    }
    Ok(exe)
}

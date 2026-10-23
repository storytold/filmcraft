//! The formatting gate rejects extra arguments even when they are not Unicode.
use std::ffi::OsString;
use std::process::Command;

#[cfg(windows)]
/// Construct an unpaired UTF-16 surrogate that cannot be converted to a Rust string.
fn hostile_argument() -> OsString {
    use std::os::windows::ffi::OsStringExt;
    OsString::from_wide(&[0xd800])
}

#[cfg(unix)]
/// Construct a non-UTF-8 argument without relying on the host locale.
fn hostile_argument() -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(vec![0xff])
}

#[cfg(any(windows, unix))]
#[test]
fn non_unicode_extra_argument_returns_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask")).arg("fmt").arg(hostile_argument()).output().expect("run xtask");
    assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("usage: cargo xtask fmt"), "{error}");
    assert!(!error.contains("panicked"), "{error}");
}

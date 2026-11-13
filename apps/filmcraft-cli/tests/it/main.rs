mod cli;
mod workflows;

/// Nextest supplies the relocated binary path when running from an archive; Cargo uses its build-time path.
fn binary() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_filmcraft_cli").unwrap_or_else(|| env!("CARGO_BIN_EXE_filmcraft-cli").into())
}

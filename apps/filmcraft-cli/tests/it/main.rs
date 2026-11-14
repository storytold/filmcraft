mod cli;
mod workflows;

fn filmcraft_command() -> std::process::Command {
    let executable = std::env::var_os("NEXTEST_BIN_EXE_filmcraft_cli").unwrap_or_else(|| env!("CARGO_BIN_EXE_filmcraft-cli").into());
    let mut command = std::process::Command::new(executable);
    // Without `--data-dir` the CLI falls back to the app's data directory, and loading the effect
    // preset library there creates folders and may rewrite files: keep tests out of the
    // developer's real FilmCraft data (#4775). An explicit `--data-dir` still wins.
    command.env("FILMCRAFT_DATA_DIR", test_data_dir());
    command
}

/// One scratch data directory shared by this test binary's CLI runs.
fn test_data_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| tempfile::Builder::new().prefix("fc-cli-data").tempdir().expect("create test data directory")).path()
}

#[test]
fn cli_executable_is_resolved() {
    assert!(std::path::Path::new(filmcraft_command().get_program()).is_file());
}

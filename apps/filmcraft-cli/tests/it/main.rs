mod cli;
mod workflows;

fn filmcraft_command() -> std::process::Command {
    // Headless sessions now load and persist preferences. Keep tests away from the user's
    // settings, including recent-project updates made when opening and saving fixtures.
    static DATA_DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let data_dir = DATA_DIR.get_or_init(|| tempfile::tempdir().expect("test data directory"));
    let executable = std::env::var_os("NEXTEST_BIN_EXE_filmcraft_cli").unwrap_or_else(|| env!("CARGO_BIN_EXE_filmcraft-cli").into());
    let mut command = std::process::Command::new(executable);
    command.env("FILMCRAFT_DATA_DIR", data_dir.path());
    command
}

#[test]
fn cli_executable_is_resolved() {
    assert!(std::path::Path::new(filmcraft_command().get_program()).is_file());
}
